//! The client side of an exec stream: relays a process to the local terminal
//! or pipes and returns its exit status.
//!
//! Output formats for tools (`needy exec --output json|ndjson`) live in the
//! CLI; this module only relays a process to local stdio. [`terminal`] owns
//! raw mode and the window size, [`signals`] forwards local signals and
//! window changes.

mod signals;
mod terminal;

use needy_api_client::exec::{Event, ExecStream, Input};
use needy_common::process::Exit;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::sync::{CancellationToken, DropGuard};

use terminal::RawMode;
pub use terminal::{interactive, restore, window};

#[derive(Debug, thiserror::Error)]
pub enum Error {
  /// The daemon refused or ended the stream, or the executor reported an
  /// error instead of an exit.
  #[error(transparent)]
  Client(#[from] needy_api_client::Error),
  #[error("cannot write the process's output")]
  Output(#[from] std::io::Error),
  /// The client got a second termination signal while the process kept
  /// running; the first was forwarded to it.
  #[error("left the process at a second {0}")]
  Terminated(&'static str),
}

impl Error {
  /// Whether the stream ended before the command exited, which a client
  /// reports as [`ENDED`](crate::signals::ENDED).
  pub fn ended(&self) -> bool {
    matches!(self, Self::Client(e) if e.kind() == needy_api_client::Kind::Ended)
  }
}

/// Bridges the local terminal or pipes to a remote process until it exits.
/// With `tty`, the local terminal is raw for the duration. A stream that
/// ends before the process exited fails, [`Error::ended`].
/// A second termination signal ends the relay with [`Error::Terminated`];
/// the terminal is restored on every way out.
pub async fn run(mut stream: ExecStream, tty: bool, stdin: bool) -> Result<Exit, Error> {
  let raw = if tty { RawMode::enable() } else { None };
  let mut forward = Forward::start(stream.input(), stdin, tty);
  let mut modes = terminal::Modes::default();
  let mut stdout = tokio::io::stdout();
  let mut stderr = tokio::io::stderr();
  let exit = loop {
    let next = tokio::select! {
      next = stream.next() => next?,
      Some(signal) = forward.terminated.recv() => return Err(Error::Terminated(signal)),
    };
    let Some(event) = next else {
      return Err(needy_api_client::Error::Ended { reason: None }.into());
    };
    match event {
      Event::Stdout(bytes) => {
        if raw.is_some() {
          modes.observe(&bytes);
        }
        stdout.write_all(&bytes).await?;
        stdout.flush().await?;
      }
      Event::Stderr(bytes) => {
        stderr.write_all(&bytes).await?;
        stderr.flush().await?;
      }
      Event::Exited(exit) => break exit,
      // A person at a terminal sees the command's output, not the trace
      // events of its placement.
      Event::Started(_) | Event::Trace(_) => {}
    }
  };
  drop(raw);
  Ok(exit)
}

/// Forwards local stdin, signals and, with `tty`, window changes to a remote
/// process. The forwarding tasks stop when this value is dropped.
pub struct Forward {
  // NOTICE: the tasks are cancelled, not awaited. A read of tokio's stdin
  // runs on a blocking thread that cannot be interrupted; the task ends at
  // once, and the thread at the next input or when the process exits.
  _stop: DropGuard,
  /// The name of a second termination signal, after which the client
  /// leaves.
  pub terminated: mpsc::Receiver<&'static str>,
}

impl Forward {
  pub fn start(input: Input, stdin: bool, tty: bool) -> Self {
    let cancel = CancellationToken::new();
    if stdin {
      tokio::spawn(forward_stdin(input.clone(), cancel.clone()));
    }
    let (terminated_tx, terminated) = mpsc::channel(1);
    tokio::spawn(signals::forward(input, tty, terminated_tx, cancel.clone()));
    Self {
      _stop: cancel.drop_guard(),
      terminated,
    }
  }
}

/// Forwards local stdin to the process, then half-closes it at end of file.
async fn forward_stdin(input: Input, cancel: CancellationToken) {
  let mut stdin = tokio::io::stdin();
  let mut buf = vec![0u8; 32 * 1024];
  loop {
    let read = tokio::select! {
      read = stdin.read(&mut buf) => read,
      () = cancel.cancelled() => return,
    };
    let n = match read {
      Ok(n) => n,
      Err(e) => {
        tracing::debug!("relay: stdin: {e}");
        0
      }
    };
    // A closed stream has nobody to tell; end of file half-closes stdin.
    let sent = if n == 0 {
      input.eof().await
    } else {
      input.stdin(buf[..n].to_vec()).await
    };
    if sent.is_err() || n == 0 {
      return;
    }
  }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
