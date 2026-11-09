//! Client side of running a process through a daemon: terminal handling,
//! stdin and signal forwarding, and the human, JSON and NDJSON output modes.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use base64::Engine;
use needy_api_client::Client;
use needy_api_proto::v1::{
    ExecAttach, ExecRequest, ExecStart, WindowSize, exec_request, exec_response,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use needy_common::process::Exit;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputMode {
    Human,
    Json,
    Ndjson,
}

pub struct ExecOptions {
    pub argv: Vec<String>,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
    pub clear_env: bool,
    pub tty: Option<bool>,
    pub no_stdin: bool,
    pub detach: bool,
    pub session: String,
    pub executor: String,
    pub output: OutputMode,
}

pub async fn connect(endpoint: &str) -> Result<Client> {
    let client = Client::connect(endpoint)
        .await
        .with_context(|| format!("cannot reach the daemon at {endpoint}"))?;
    Ok(match std::env::var("NEEDY_TOKEN") {
        Ok(token) if !token.is_empty() => client.with_token(&token)?,
        _ => client,
    })
}

pub async fn exec(endpoint: &str, options: ExecOptions) -> Result<i32> {
    let interactive = stdin_is_tty() && stdout_is_tty();
    let tty = options.tty.unwrap_or(interactive) && options.output == OutputMode::Human;
    let window = if tty { Some(window()) } else { None };
    let mut env: std::collections::HashMap<String, String> = options.env.into_iter().collect();
    if tty && let Ok(term) = std::env::var("TERM") {
        env.entry("TERM".into()).or_insert(term);
    }
    let start = ExecStart {
        argv: options.argv,
        cwd: options.cwd.unwrap_or_default(),
        env,
        clear_env: options.clear_env,
        tty: window,
        detach: options.detach,
        session_id: options.session,
        executor: options.executor,
        no_stdin: options.no_stdin || options.detach,
        origin: "cli".into(),
    };
    let first = ExecRequest {
        payload: Some(exec_request::Payload::Start(start)),
    };
    let client = connect(endpoint).await?;
    let exit = run_stream(
        client,
        first,
        tty,
        !(options.no_stdin || options.detach),
        options.detach,
        options.output,
    )
    .await?;
    Ok(crate::signals::shell_code(&exit))
}

pub async fn attach(endpoint: &str, process_id: String, replay: bool) -> Result<i32> {
    let client = connect(endpoint).await?;
    let first = ExecRequest {
        payload: Some(exec_request::Payload::Attach(ExecAttach {
            process_id,
            replay,
        })),
    };
    let tty = stdin_is_tty() && stdout_is_tty();
    let exit = run_stream(client, first, tty, true, false, OutputMode::Human).await?;
    Ok(crate::signals::shell_code(&exit))
}

/// Bridges the local terminal or pipes to a remote process until it exits.
pub async fn run_stream(
    client: Client,
    first: ExecRequest,
    tty: bool,
    forward_stdin: bool,
    detach: bool,
    output: OutputMode,
) -> Result<Exit> {
    let (tx, rx) = mpsc::channel::<ExecRequest>(64);
    tx.send(first).await?;
    let mut responses = client
        .processes()
        .exec(ReceiverStream::new(rx))
        .await
        .map_err(|s| anyhow::anyhow!("{}", s.message()))?
        .into_inner();

    let _terminal = if tty { Terminal::raw() } else { None };
    if forward_stdin {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut stdin = tokio::io::stdin();
            let mut buf = vec![0u8; 32 * 1024];
            loop {
                match stdin.read(&mut buf).await {
                    Ok(0) | Err(_) => {
                        let _ = tx
                            .send(ExecRequest {
                                payload: Some(exec_request::Payload::StdinEof(true)),
                            })
                            .await;
                        break;
                    }
                    Ok(n) => {
                        if tx
                            .send(ExecRequest {
                                payload: Some(exec_request::Payload::Stdin(buf[..n].to_vec())),
                            })
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        });
    }
    forward_signals(tx.clone(), tty);

    let mut stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();
    let mut collected_out = Vec::new();
    let mut collected_err = Vec::new();
    let mut summary: BTreeMap<&str, Value> = BTreeMap::new();
    let mut exit = Exit {
        code: Some(0),
        signal: String::new(),
    };
    while let Some(message) = responses
        .message()
        .await
        .map_err(|s| anyhow::anyhow!("{}", s.message()))?
    {
        match message.payload {
            Some(exec_response::Payload::Started(started)) => {
                if output == OutputMode::Ndjson {
                    line(
                        &json!({"type": "started", "process_id": started.process_id, "executor": started.executor, "pid": started.pid, "argv": started.argv}),
                    )?;
                }
                summary.insert("process_id", json!(started.process_id));
                summary.insert("executor", json!(started.executor));
                if detach {
                    match output {
                        OutputMode::Human => println!("{}", started.process_id),
                        OutputMode::Json => line(&json!(summary))?,
                        OutputMode::Ndjson => {}
                    }
                    return Ok(exit);
                }
            }
            Some(exec_response::Payload::Stdout(bytes)) => match output {
                OutputMode::Human => {
                    stdout.write_all(&bytes).await?;
                    stdout.flush().await?;
                }
                OutputMode::Json => collected_out.extend_from_slice(&bytes),
                OutputMode::Ndjson => line(&chunk("stdout", &bytes))?,
            },
            Some(exec_response::Payload::Stderr(bytes)) => match output {
                OutputMode::Human => {
                    stderr.write_all(&bytes).await?;
                    stderr.flush().await?;
                }
                OutputMode::Json => collected_err.extend_from_slice(&bytes),
                OutputMode::Ndjson => line(&chunk("stderr", &bytes))?,
            },
            Some(exec_response::Payload::Exited(exited)) => {
                summary.insert("exit_code", json!(exited.code));
                summary.insert("signal", json!(exited.signal));
                if output == OutputMode::Ndjson {
                    line(&json!({"type": "exit", "code": exited.code, "signal": exited.signal}))?;
                }
                exit = Exit {
                    code: exited.code,
                    signal: exited.signal,
                };
                break;
            }
            Some(exec_response::Payload::Error(error)) => bail!("{error}"),
            None => {}
        }
    }
    drop(_terminal);
    if output == OutputMode::Json {
        summary.insert(
            "stdout",
            json!(String::from_utf8_lossy(&collected_out).to_string()),
        );
        summary.insert(
            "stderr",
            json!(String::from_utf8_lossy(&collected_err).to_string()),
        );
        line(&json!(summary))?;
    }
    Ok(exit)
}

fn chunk(kind: &str, bytes: &[u8]) -> Value {
    match std::str::from_utf8(bytes) {
        Ok(text) => json!({"type": kind, "data": text}),
        Err(_) => {
            json!({"type": kind, "data_base64": base64::engine::general_purpose::STANDARD.encode(bytes)})
        }
    }
}

fn line(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

#[cfg(unix)]
fn forward_signals(tx: mpsc::Sender<ExecRequest>, tty: bool) {
    use tokio::signal::unix::{SignalKind, signal};
    tokio::spawn(async move {
        let (Ok(mut int), Ok(mut term), Ok(mut hup), Ok(mut quit), Ok(mut winch)) = (
            signal(SignalKind::interrupt()),
            signal(SignalKind::terminate()),
            signal(SignalKind::hangup()),
            signal(SignalKind::quit()),
            signal(SignalKind::window_change()),
        ) else {
            return;
        };
        loop {
            let request = tokio::select! {
                _ = int.recv() => exec_request::Payload::Signal("SIGINT".into()),
                _ = term.recv() => exec_request::Payload::Signal("SIGTERM".into()),
                _ = hup.recv() => exec_request::Payload::Signal("SIGHUP".into()),
                _ = quit.recv() => exec_request::Payload::Signal("SIGQUIT".into()),
                _ = winch.recv() => {
                    if !tty { continue; }
                    exec_request::Payload::Resize(window())
                }
            };
            if tx
                .send(ExecRequest {
                    payload: Some(request),
                })
                .await
                .is_err()
            {
                break;
            }
        }
    });
}

#[cfg(not(unix))]
fn forward_signals(_tx: mpsc::Sender<ExecRequest>, _tty: bool) {}

#[cfg(unix)]
fn stdin_is_tty() -> bool {
    unsafe { libc::isatty(0) == 1 }
}

#[cfg(unix)]
fn stdout_is_tty() -> bool {
    unsafe { libc::isatty(1) == 1 }
}

#[cfg(not(unix))]
fn stdin_is_tty() -> bool {
    false
}

#[cfg(not(unix))]
fn stdout_is_tty() -> bool {
    false
}

#[cfg(unix)]
pub fn window() -> WindowSize {
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(0, libc::TIOCGWINSZ as _, &mut size) } != 0 {
        return WindowSize { rows: 24, cols: 80 };
    }
    WindowSize {
        rows: u32::from(size.ws_row.max(1)),
        cols: u32::from(size.ws_col.max(1)),
    }
}

#[cfg(not(unix))]
pub fn window() -> WindowSize {
    WindowSize { rows: 24, cols: 80 }
}

/// Puts the local terminal in raw mode and restores it on drop.
pub struct Terminal {
    #[cfg(unix)]
    original: libc::termios,
}

impl Terminal {
    #[cfg(unix)]
    pub fn raw() -> Option<Self> {
        let mut original: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(0, &mut original) } != 0 {
            return None;
        }
        let mut raw = original;
        unsafe { libc::cfmakeraw(&mut raw) };
        if unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw) } != 0 {
            return None;
        }
        Some(Self { original })
    }

    #[cfg(not(unix))]
    pub fn raw() -> Option<Self> {
        None
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &self.original);
        }
    }
}
