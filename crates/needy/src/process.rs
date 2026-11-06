use crate::wire;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Window {
    pub rows: u16,
    pub cols: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Exit {
    pub code: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
}

fn signals() -> [(&'static str, i32); 22] {
    [
        ("SIGHUP", libc::SIGHUP),
        ("SIGINT", libc::SIGINT),
        ("SIGQUIT", libc::SIGQUIT),
        ("SIGILL", libc::SIGILL),
        ("SIGTRAP", libc::SIGTRAP),
        ("SIGABRT", libc::SIGABRT),
        ("SIGFPE", libc::SIGFPE),
        ("SIGKILL", libc::SIGKILL),
        ("SIGBUS", libc::SIGBUS),
        ("SIGSEGV", libc::SIGSEGV),
        ("SIGSYS", libc::SIGSYS),
        ("SIGPIPE", libc::SIGPIPE),
        ("SIGALRM", libc::SIGALRM),
        ("SIGTERM", libc::SIGTERM),
        ("SIGXCPU", libc::SIGXCPU),
        ("SIGXFSZ", libc::SIGXFSZ),
        ("SIGVTALRM", libc::SIGVTALRM),
        ("SIGPROF", libc::SIGPROF),
        ("SIGUSR1", libc::SIGUSR1),
        ("SIGUSR2", libc::SIGUSR2),
        ("SIGIO", libc::SIGIO),
        ("SIGURG", libc::SIGURG),
    ]
}
pub fn signal_number(name: &str) -> Result<i32> {
    signals()
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, number)| *number)
        .context("unsupported remote signal")
}

impl Exit {
    pub fn shell_code(&self) -> Result<i32> {
        Ok(match &self.signal {
            Some(signal) => 128 + signal_number(signal)?,
            None => self.code,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub command: Vec<String>,
    #[serde(default)]
    pub argv0: Option<String>,
    pub cwd: PathBuf,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    pub pty: Option<Window>,
}

pub fn validate(request: &Request) -> Result<()> {
    ensure!(
        !request.command.is_empty() && request.command.len() <= 32768,
        "invalid command argument count"
    );
    ensure!(
        request.command.iter().all(|value| !value.contains('\0')) && !request.command[0].is_empty(),
        "invalid command arguments"
    );
    ensure!(
        request
            .argv0
            .as_ref()
            .is_none_or(|value| !value.contains('\0')),
        "invalid argv[0]"
    );
    ensure!(
        request.command.iter().map(String::len).sum::<usize>() < 2 << 20,
        "command exceeds 2 MiB"
    );
    for (key, value) in &request.environment {
        ensure!(
            !key.is_empty() && !key.contains(['=', '\0']) && !value.contains('\0'),
            "invalid environment entry"
        );
        ensure!(
            !matches!(
                key.as_str(),
                "PATH"
                    | "HOME"
                    | "LD_PRELOAD"
                    | "LD_LIBRARY_PATH"
                    | "DYLD_INSERT_LIBRARIES"
                    | "DYLD_LIBRARY_PATH"
            ),
            "environment variable is not forwardable"
        );
    }
    if let Some(window) = request.pty {
        ensure!(window.rows > 0 && window.cols > 0, "invalid terminal size");
    }
    Ok(())
}

struct Group(i32);
impl Drop for Group {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-self.0, libc::SIGKILL);
        }
    }
}

fn terminal(window: Window) -> Result<(Arc<AsyncFd<File>>, File)> {
    let mut master = -1;
    let mut slave = -1;
    let mut size = libc::winsize {
        ws_row: window.rows,
        ws_col: window.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    ensure!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::addr_of_mut!(size),
            )
        } == 0,
        "openpty failed: {}",
        std::io::Error::last_os_error()
    );
    let master = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    for file in [&master, &slave] {
        ensure!(
            unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } != -1,
            "cannot mark PTY close-on-exec"
        );
    }
    ensure!(
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } != -1,
        "cannot make PTY nonblocking"
    );
    // The owned File stays open and is never replaced while registered.
    Ok((Arc::new(unsafe { AsyncFd::register(master)? }), slave))
}

async fn read_pty(master: &AsyncFd<File>, bytes: &mut [u8]) -> std::io::Result<usize> {
    loop {
        let mut ready = master.readable().await?;
        match ready.try_io(|fd| {
            let count =
                unsafe { libc::read(fd.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len()) };
            if count < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(count as usize)
            }
        }) {
            Ok(Err(error)) if error.raw_os_error() == Some(libc::EIO) => return Ok(0),
            Ok(result) => return result,
            Err(_) => continue,
        }
    }
}

async fn write_pty(master: &AsyncFd<File>, mut bytes: &[u8]) -> Result<()> {
    while !bytes.is_empty() {
        let mut ready = master.writable().await?;
        match ready.try_io(|fd| {
            let count = unsafe { libc::write(fd.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
            if count < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(count as usize)
            }
        }) {
            Ok(result) => {
                let count = result?;
                ensure!(count != 0, "PTY write returned zero");
                bytes = &bytes[count..];
            }
            Err(_) => continue,
        }
    }
    Ok(())
}

async fn copy_output(
    mut reader: impl AsyncRead + Unpin,
    kind: u8,
    sender: mpsc::Sender<(u8, Vec<u8>)>,
) -> Result<()> {
    let mut buffer = vec![0; 32768];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            return Ok(());
        }
        if sender.send((kind, buffer[..count].to_vec())).await.is_err() {
            return Ok(());
        }
    }
}

fn spawn(request: &Request) -> Result<(Child, Option<Arc<AsyncFd<File>>>)> {
    let mut command = Command::new(&request.command[0]);
    command
        .args(&request.command[1..])
        .current_dir(&request.cwd)
        .envs(&request.environment)
        .kill_on_drop(true);
    if let Some(argv0) = &request.argv0 {
        command.as_std_mut().arg0(argv0);
    }
    let mut master = None;
    if let Some(window) = request.pty {
        let (pty, slave) = terminal(window)?;
        command
            .stdin(slave.try_clone()?)
            .stdout(slave.try_clone()?)
            .stderr(slave);
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        master = Some(pty);
    } else {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.as_std_mut().process_group(0);
    }
    Ok((
        command.spawn().context("cannot start host command")?,
        master,
    ))
}

/// A separate process owns the host child. Loss of its daemon pipe is a
/// cancellation, including when the daemon is killed without running cleanup.
pub async fn worker(request: Request) -> Result<i32> {
    validate(&request)?;
    let (mut child, master) = spawn(&request)?;
    let pid = child.id().context("host child has no PID")? as i32;
    let _group = Group(pid);
    let mut stdin = child.stdin.take();
    let (sender, mut receiver) = mpsc::channel(32);
    let mut readers = Vec::new();
    if let Some(master) = master.clone() {
        let sender = sender.clone();
        readers.push(tokio::spawn(async move {
            let mut buffer = vec![0; 32768];
            loop {
                let count = read_pty(&master, &mut buffer).await?;
                if count == 0 {
                    return Ok::<_, anyhow::Error>(());
                }
                if sender
                    .send((wire::STDOUT, buffer[..count].to_vec()))
                    .await
                    .is_err()
                {
                    return Ok(());
                }
            }
        }));
    } else {
        readers.push(tokio::spawn(copy_output(
            child.stdout.take().unwrap(),
            wire::STDOUT,
            sender.clone(),
        )));
        readers.push(tokio::spawn(copy_output(
            child.stderr.take().unwrap(),
            wire::STDERR,
            sender.clone(),
        )));
    }
    drop(sender);
    let writer = tokio::spawn(async move {
        let mut output = tokio::io::stdout();
        while let Some((kind, bytes)) = receiver.recv().await {
            wire::write(&mut output, kind, &bytes).await?;
        }
        Ok::<_, anyhow::Error>(())
    });
    let input = async {
        let mut input = tokio::io::stdin();
        let mut eof = false;
        loop {
            let (kind, bytes) = wire::read(&mut input).await?;
            match kind {
                wire::STDIN => {
                    ensure!(!eof, "input after stdin EOF");
                    if let Some(master) = &master {
                        write_pty(master, &bytes).await?;
                    } else {
                        stdin
                            .as_mut()
                            .context("stdin is closed")?
                            .write_all(&bytes)
                            .await?;
                    }
                }
                wire::EOF => {
                    ensure!(!eof && bytes.is_empty(), "invalid stdin EOF");
                    eof = true;
                    if let Some(master) = &master {
                        write_pty(master, &[4]).await?;
                    } else if let Some(mut pipe) = stdin.take() {
                        pipe.shutdown().await?;
                    }
                }
                wire::SIGNAL => {
                    let signal: i32 = serde_json::from_slice(&bytes)?;
                    ensure!(
                        matches!(
                            signal,
                            libc::SIGINT | libc::SIGTERM | libc::SIGHUP | libc::SIGQUIT
                        ),
                        "unsupported signal"
                    );
                    if unsafe { libc::kill(-pid, signal) } != 0
                        && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
                    {
                        return Err(std::io::Error::last_os_error().into());
                    }
                }
                wire::RESIZE => {
                    let window: Window = serde_json::from_slice(&bytes)?;
                    ensure!(window.rows > 0 && window.cols > 0, "invalid terminal size");
                    let terminal = master.as_ref().context("process has no PTY")?;
                    let size = libc::winsize {
                        ws_row: window.rows,
                        ws_col: window.cols,
                        ws_xpixel: 0,
                        ws_ypixel: 0,
                    };
                    ensure!(
                        unsafe { libc::ioctl(terminal.as_raw_fd(), libc::TIOCSWINSZ as _, &size) }
                            == 0,
                        "cannot resize PTY"
                    );
                }
                _ => bail!("invalid client process frame"),
            }
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    };
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let result = tokio::select! {
        result = child.wait() => result?,
        error = input => { let _ = unsafe { libc::kill(-pid, libc::SIGKILL) }; child.wait().await?; error?; bail!("host connection closed"); },
        _ = terminate.recv() => { unsafe { libc::kill(-pid, libc::SIGKILL); } child.wait().await? },
    };
    // Close orphaned children that still hold the standard streams open.
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    for reader in readers {
        reader.await??;
    }
    writer.await??;
    let status = result;
    let code = status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(libc::SIGKILL));
    let signal = status
        .signal()
        .map(|number| {
            signals()
                .iter()
                .find(|(_, value)| *value == number)
                .map(|(name, _)| (*name).to_owned())
                .context("unrepresentable process signal")
        })
        .transpose()?;
    wire::write(
        &mut tokio::io::stdout(),
        wire::EXIT,
        &serde_json::to_vec(&Exit { code, signal })?,
    )
    .await?;
    Ok(code)
}
