//! Runs processes on this machine and keeps them independent of clients.
//!
//! A process belongs to the host, not to the stream that started it. Output is
//! fanned out to attached clients and kept in a bounded replay buffer, so a
//! client may disconnect and attach again.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
pub use needy_common::process::Exit;

use crate::signals;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, watch};

/// Replay buffer per process.
const REPLAY_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone)]
pub enum Output {
    Stdout(Bytes),
    Stderr(Bytes),
    Exit(Exit),
}

#[derive(Debug)]
pub enum Input {
    Stdin(Vec<u8>),
    Eof,
    /// Signal name, for example `SIGINT`.
    Signal(String),
    Resize(u16, u16),
}

#[derive(Debug, Clone, Default)]
pub struct Spec {
    pub argv: Vec<String>,
    pub cwd: String,
    pub env: Vec<(String, String)>,
    pub clear_env: bool,
    pub tty: Option<(u16, u16)>,
    pub no_stdin: bool,
    pub session_id: String,
}

struct Replay {
    events: VecDeque<Output>,
    bytes: usize,
    finished: Option<Exit>,
}

pub struct Proc {
    pub id: String,
    pub argv: Vec<String>,
    pub pid: i64,
    pub tty: bool,
    pub started_at_ms: i64,
    pub session_id: String,
    input: mpsc::UnboundedSender<Input>,
    subscribers: tokio::sync::Mutex<Vec<mpsc::Sender<Output>>>,
    replay: Mutex<Replay>,
    exited: watch::Receiver<Option<Exit>>,
}

impl Proc {
    pub fn send(&self, input: Input) {
        let _ = self.input.send(input);
    }

    pub fn exit(&self) -> Option<Exit> {
        self.exited.borrow().clone()
    }

    pub async fn wait(&self) -> Exit {
        let mut exited = self.exited.clone();
        loop {
            if let Some(exit) = exited.borrow().clone() {
                return exit;
            }
            if exited.changed().await.is_err() {
                return Exit {
                    code: None,
                    signal: "SIGKILL".into(),
                };
            }
        }
    }

    /// Subscribes to output. With `replay`, buffered output comes first. The
    /// subscriber lock orders the snapshot before any live event. A slow
    /// subscriber slows the process down instead of losing output.
    pub async fn subscribe(&self, replay: bool) -> (Vec<Output>, mpsc::Receiver<Output>) {
        let mut subscribers = self.subscribers.lock().await;
        let (tx, rx) = mpsc::channel(64);
        let state = self.replay.lock().unwrap();
        let mut past: Vec<Output> = if replay {
            state.events.iter().cloned().collect()
        } else {
            Vec::new()
        };
        if let Some(exit) = &state.finished {
            past.push(Output::Exit(exit.clone()));
        } else {
            subscribers.push(tx);
        }
        (past, rx)
    }

    async fn emit(&self, event: Output) {
        let mut subscribers = self.subscribers.lock().await;
        self.record(&event);
        let mut alive = Vec::with_capacity(subscribers.len());
        for tx in subscribers.drain(..) {
            if tx.send(event.clone()).await.is_ok() {
                alive.push(tx);
            }
        }
        if !matches!(event, Output::Exit(_)) {
            *subscribers = alive;
        }
    }

    fn record(&self, event: &Output) {
        let mut state = self.replay.lock().unwrap();
        match event {
            Output::Stdout(b) | Output::Stderr(b) => {
                state.bytes += b.len();
                state.events.push_back(event.clone());
                while state.bytes > REPLAY_BYTES {
                    match state.events.pop_front() {
                        Some(Output::Stdout(b) | Output::Stderr(b)) => state.bytes -= b.len(),
                        Some(_) => {}
                        None => break,
                    }
                }
            }
            Output::Exit(exit) => state.finished = Some(exit.clone()),
        }
    }
}

#[derive(Clone, Default)]
pub struct Host {
    procs: Arc<Mutex<HashMap<String, Arc<Proc>>>>,
}

impl Host {
    pub fn get(&self, id: &str) -> Option<Arc<Proc>> {
        self.procs.lock().unwrap().get(id).cloned()
    }

    pub fn list(&self) -> Vec<Arc<Proc>> {
        let mut all: Vec<_> = self.procs.lock().unwrap().values().cloned().collect();
        all.sort_by_key(|p| p.started_at_ms);
        all
    }

    pub fn spawn(&self, spec: Spec) -> Result<Arc<Proc>> {
        if spec.argv.is_empty() || spec.argv[0].is_empty() {
            bail!("empty command");
        }
        let proc = match spec.tty {
            Some(size) => spawn_pty(&spec, size)?,
            None => spawn_pipes(&spec)?,
        };
        self.procs
            .lock()
            .unwrap()
            .insert(proc.id.clone(), proc.clone());
        // Forget finished processes after a while so the table stays small.
        let procs = self.procs.clone();
        let tracked = proc.clone();
        tokio::spawn(async move {
            tracked.wait().await;
            tokio::time::sleep(std::time::Duration::from_secs(600)).await;
            procs.lock().unwrap().remove(&tracked.id);
        });
        Ok(proc)
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn new_proc(
    spec: &Spec,
    pid: i64,
    tty: bool,
) -> (
    Arc<Proc>,
    mpsc::UnboundedReceiver<Input>,
    watch::Sender<Option<Exit>>,
) {
    let (input, input_rx) = mpsc::unbounded_channel();
    let (exit_tx, exited) = watch::channel(None);
    let proc = Arc::new(Proc {
        id: uuid::Uuid::new_v4().simple().to_string()[..12].to_string(),
        argv: spec.argv.clone(),
        pid,
        tty,
        started_at_ms: now_ms(),
        session_id: spec.session_id.clone(),
        input,
        subscribers: tokio::sync::Mutex::new(Vec::new()),
        replay: Mutex::new(Replay {
            events: VecDeque::new(),
            bytes: 0,
            finished: None,
        }),
        exited,
    });
    (proc, input_rx, exit_tx)
}

async fn finish(proc: &Proc, exit_tx: &watch::Sender<Option<Exit>>, exit: Exit) {
    proc.emit(Output::Exit(exit.clone())).await;
    let _ = exit_tx.send(Some(exit));
}

fn spawn_pipes(spec: &Spec) -> Result<Arc<Proc>> {
    let mut command = tokio::process::Command::new(&spec.argv[0]);
    command.args(&spec.argv[1..]);
    if !spec.cwd.is_empty() {
        command.current_dir(&spec.cwd);
    }
    if spec.clear_env {
        command.env_clear();
    }
    command.envs(spec.env.iter().cloned());
    command.stdin(if spec.no_stdin {
        std::process::Stdio::null()
    } else {
        std::process::Stdio::piped()
    });
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .with_context(|| format!("cannot start {}", spec.argv[0]))?;
    let pid = child.id().map(i64::from).unwrap_or(-1);
    let (proc, mut input_rx, exit_tx) = new_proc(spec, pid, false);

    let mut stdin = child.stdin.take();
    let mut stdout = child.stdout.take().context("stdout")?;
    let mut stderr = child.stderr.take().context("stderr")?;
    let out_proc = proc.clone();
    let out = tokio::spawn(async move {
        let mut buf = vec![0u8; 256 * 1024];
        while let Ok(n) = stdout.read(&mut buf).await {
            if n == 0 {
                break;
            }
            out_proc
                .emit(Output::Stdout(Bytes::copy_from_slice(&buf[..n])))
                .await;
        }
    });
    let err_proc = proc.clone();
    let err = tokio::spawn(async move {
        let mut buf = vec![0u8; 256 * 1024];
        while let Ok(n) = stderr.read(&mut buf).await {
            if n == 0 {
                break;
            }
            err_proc
                .emit(Output::Stderr(Bytes::copy_from_slice(&buf[..n])))
                .await;
        }
    });
    tokio::spawn(async move {
        while let Some(input) = input_rx.recv().await {
            match input {
                Input::Stdin(bytes) => {
                    if let Some(s) = stdin.as_mut()
                        && s.write_all(&bytes).await.is_err()
                    {
                        stdin = None;
                    }
                }
                Input::Eof => stdin = None,
                Input::Signal(sig) => signal_group(pid, &sig),
                Input::Resize(..) => {}
            }
        }
    });
    let wait_proc = proc.clone();
    tokio::spawn(async move {
        let status = child.wait().await;
        let _ = out.await;
        let _ = err.await;
        let exit = match status {
            Ok(status) => exit_from_std(status),
            Err(_) => Exit {
                code: None,
                signal: "SIGKILL".into(),
            },
        };
        finish(&wait_proc, &exit_tx, exit).await;
    });
    Ok(proc)
}

fn spawn_pty(spec: &Spec, (rows, cols): (u16, u16)) -> Result<Arc<Proc>> {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    let pair = native_pty_system()
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("cannot open a terminal")?;
    let mut command = CommandBuilder::new(&spec.argv[0]);
    command.args(&spec.argv[1..]);
    if !spec.cwd.is_empty() {
        command.cwd(&spec.cwd);
    }
    if spec.clear_env {
        command.env_clear();
    }
    for (k, v) in &spec.env {
        command.env(k, v);
    }
    let mut child = pair
        .slave
        .spawn_command(command)
        .with_context(|| format!("cannot start {}", spec.argv[0]))?;
    drop(pair.slave);
    let pid = child.process_id().map(i64::from).unwrap_or(-1);
    let (proc, mut input_rx, exit_tx) = new_proc(spec, pid, true);
    let master = pair.master;
    let mut reader = master.try_clone_reader().context("terminal reader")?;
    let mut writer = master.take_writer().context("terminal writer")?;

    let (out_tx, mut out_rx) = mpsc::channel::<Bytes>(64);
    let reader_thread = std::thread::spawn(move || {
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if out_tx
                        .blocking_send(Bytes::copy_from_slice(&buf[..n]))
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });
    let out_proc = proc.clone();
    let pump = tokio::spawn(async move {
        while let Some(bytes) = out_rx.recv().await {
            out_proc.emit(Output::Stdout(bytes)).await;
        }
    });
    let (in_tx, in_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        while let Ok(bytes) = in_rx.recv() {
            if writer.write_all(&bytes).is_err() {
                break;
            }
            let _ = writer.flush();
        }
    });
    // The master is dropped after the child exits: ConPTY on Windows only ends
    // the reader once its pseudo console closes.
    let master = Arc::new(Mutex::new(Some(master)));
    let wait_master = master.clone();
    tokio::spawn(async move {
        while let Some(input) = input_rx.recv().await {
            match input {
                Input::Stdin(bytes) => {
                    let _ = in_tx.send(bytes);
                }
                // A terminal has no half-close; send the EOF character.
                Input::Eof => {
                    let _ = in_tx.send(vec![4]);
                }
                Input::Signal(sig) => signal_group(pid, &sig),
                Input::Resize(rows, cols) => {
                    if let Some(master) = master.lock().unwrap().as_ref() {
                        let _ = master.resize(PtySize {
                            rows,
                            cols,
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                    }
                }
            }
        }
    });
    let wait_proc = proc.clone();
    tokio::task::spawn_blocking(move || {
        let exit = wait_pty_child(&mut child, pid);
        // On Unix the reader ends when the child closes the terminal; give it
        // a moment to drain before closing the master, which Windows needs.
        for _ in 0..50 {
            if reader_thread.is_finished() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        wait_master.lock().unwrap().take();
        let _ = reader_thread.join();
        tokio::runtime::Handle::current().block_on(async move {
            let _ = pump.await;
            finish(&wait_proc, &exit_tx, exit).await;
        });
    });
    Ok(proc)
}

/// Waits for a terminal's child. On Unix this reaps it directly, so a signal
/// death keeps its exact signal instead of a description.
#[cfg(unix)]
fn wait_pty_child(_child: &mut Box<dyn portable_pty::Child + Send + Sync>, pid: i64) -> Exit {
    use nix::sys::wait::{WaitStatus, waitpid};
    loop {
        match waitpid(nix::unistd::Pid::from_raw(pid as i32), None) {
            Ok(WaitStatus::Exited(_, code)) => {
                return Exit {
                    code: Some(code),
                    signal: String::new(),
                };
            }
            Ok(WaitStatus::Signaled(_, sig, _)) => {
                return Exit {
                    code: None,
                    signal: sig.as_str().to_string(),
                };
            }
            Ok(_) | Err(nix::errno::Errno::EINTR) => continue,
            Err(_) => {
                return Exit {
                    code: None,
                    signal: "SIGKILL".into(),
                };
            }
        }
    }
}

#[cfg(not(unix))]
fn wait_pty_child(child: &mut Box<dyn portable_pty::Child + Send + Sync>, _pid: i64) -> Exit {
    match child.wait() {
        Ok(status) => Exit {
            code: Some(status.exit_code() as i32),
            signal: String::new(),
        },
        Err(_) => Exit {
            code: None,
            signal: "SIGKILL".into(),
        },
    }
}

#[cfg(unix)]
fn exit_from_std(status: std::process::ExitStatus) -> Exit {
    use std::os::unix::process::ExitStatusExt;
    match status.signal() {
        Some(sig) => Exit {
            code: None,
            signal: signals::name(sig),
        },
        None => Exit {
            code: status.code(),
            signal: String::new(),
        },
    }
}

#[cfg(not(unix))]
fn exit_from_std(status: std::process::ExitStatus) -> Exit {
    Exit {
        code: status.code(),
        signal: String::new(),
    }
}

#[cfg(unix)]
fn signal_group(pid: i64, name: &str) {
    use nix::sys::signal::{Signal, kill, killpg};
    use nix::unistd::Pid;
    let Ok(sig) = name.parse::<Signal>() else {
        return;
    };
    if pid > 0 {
        // The child leads its own process group (pipes) or session (PTY).
        let pid = Pid::from_raw(pid as i32);
        if killpg(pid, sig).is_err() {
            let _ = kill(pid, sig);
        }
    }
}

#[cfg(not(unix))]
fn signal_group(pid: i64, _name: &str) {
    // Windows has no POSIX signals; any signal terminates the process.
    if pid > 0 {
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .status();
    }
}
