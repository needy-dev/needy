//! Client side of running a process through a daemon: the terminal, stdin and
//! signal forwarding, and relaying a process's output to local stdio.
//!
//! Output formats for tools (`needy exec --output json|ndjson`) live in the
//! CLI; this module only relays a process to the local terminal or pipes.

use anyhow::{Context, Result, bail};
use needy_api_client::Client;
use needy_api_proto::v1::{ExecRequest, ExecResponse, WindowSize, exec_request, exec_response};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use needy_common::process::Exit;

pub async fn connect(endpoint: &str) -> Result<Client> {
  let client = Client::connect(endpoint).await.with_context(|| format!("cannot reach the daemon at {endpoint}"))?;
  Ok(match std::env::var("NEEDY_TOKEN") {
    Ok(token) if !token.is_empty() => client.with_token(&token)?,
    _ => client,
  })
}

/// Opens an exec stream whose first request is `first`. The error is the
/// daemon's status, so callers can tell a refused command from a failure.
pub async fn open(
  client: &Client,
  first: ExecRequest,
) -> Result<(mpsc::Sender<ExecRequest>, tonic::Streaming<ExecResponse>), tonic::Status> {
  let (tx, rx) = mpsc::channel::<ExecRequest>(64);
  tx.send(first).await.map_err(|_| tonic::Status::internal("exec request channel closed"))?;
  let responses = client.processes().exec(ReceiverStream::new(rx)).await?.into_inner();
  Ok((tx, responses))
}

/// Opens an exec stream and relays it to local stdio until the process exits.
pub async fn run_stream(client: Client, first: ExecRequest, tty: bool, forward_stdin: bool) -> Result<Exit> {
  let (tx, responses) = open(&client, first).await.map_err(|s| anyhow::anyhow!("{}", s.message()))?;
  relay(tx, responses, tty, forward_stdin).await
}

/// Bridges the local terminal or pipes to a remote process until it exits.
pub async fn relay(tx: mpsc::Sender<ExecRequest>, mut responses: tonic::Streaming<ExecResponse>, tty: bool, stdin: bool) -> Result<Exit> {
  let terminal = if tty { Terminal::raw() } else { None };
  if stdin {
    forward_stdin(tx.clone());
  }
  forward_signals(tx.clone(), tty);

  let mut stdout = tokio::io::stdout();
  let mut stderr = tokio::io::stderr();
  let mut exit = Exit {
    code: Some(0),
    signal: String::new(),
  };
  while let Some(message) = responses.message().await.map_err(|s| anyhow::anyhow!("{}", s.message()))? {
    match message.payload {
      Some(exec_response::Payload::Stdout(bytes)) => {
        stdout.write_all(&bytes).await?;
        stdout.flush().await?;
      }
      Some(exec_response::Payload::Stderr(bytes)) => {
        stderr.write_all(&bytes).await?;
        stderr.flush().await?;
      }
      Some(exec_response::Payload::Exited(exited)) => {
        exit = Exit {
          code: exited.code,
          signal: exited.signal,
        };
        break;
      }
      Some(exec_response::Payload::Error(error)) => bail!("{error}"),
      Some(exec_response::Payload::Started(_)) | None => {}
    }
  }
  drop(terminal);
  Ok(exit)
}

/// Forwards local stdin to the process, then half-closes it at end of file.
pub fn forward_stdin(tx: mpsc::Sender<ExecRequest>) {
  tokio::spawn(async move {
    let mut stdin = tokio::io::stdin();
    let mut buf = vec![0u8; 32 * 1024];
    loop {
      let payload = match stdin.read(&mut buf).await {
        Ok(0) | Err(_) => exec_request::Payload::StdinEof(true),
        Ok(n) => exec_request::Payload::Stdin(buf[..n].to_vec()),
      };
      let eof = matches!(payload, exec_request::Payload::StdinEof(_));
      // A closed channel means the stream ended; there is nobody to tell.
      if tx
        .send(ExecRequest {
          payload: Some(payload),
        })
        .await
        .is_err()
        || eof
      {
        break;
      }
    }
  });
}

/// Forwards SIGINT, SIGTERM, SIGHUP and SIGQUIT, and window changes when `tty`.
#[cfg(unix)]
pub fn forward_signals(tx: mpsc::Sender<ExecRequest>, tty: bool) {
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
pub fn forward_signals(_tx: mpsc::Sender<ExecRequest>, _tty: bool) {}

#[cfg(unix)]
pub fn stdin_is_tty() -> bool {
  unsafe { libc::isatty(0) == 1 }
}

#[cfg(unix)]
pub fn stdout_is_tty() -> bool {
  unsafe { libc::isatty(1) == 1 }
}

#[cfg(not(unix))]
pub fn stdin_is_tty() -> bool {
  false
}

#[cfg(not(unix))]
pub fn stdout_is_tty() -> bool {
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
