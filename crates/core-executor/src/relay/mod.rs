//! The client side of an exec stream: relays a process to the local terminal
//! or pipes and returns its exit status.
//!
//! Output formats for tools (`needy exec --output json|ndjson`) live in the
//! CLI; this module only relays a process to local stdio. [`terminal`] owns
//! raw mode and the window size, [`signals`] forwards local signals and
//! window changes, [`keys`] finds the detach keys in what the user types.

mod keys;
mod signals;
mod stdin;
mod terminal;

use std::time::Duration;

use needy_api_client::exec::{Event, ExecStream, Input};
use needy_common::process::Exit;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio_util::sync::{CancellationToken, DropGuard};

pub use keys::{Invalid as InvalidDetachKeys, Scanner as DetachKeys};
use terminal::RawMode;
pub use terminal::{interactive, restore, window};

/// The detach keys clients use unless told otherwise.
pub const DEFAULT_DETACH_KEYS: &str = "ctrl-]";

/// How a relay ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
  Exited(Exit),
  /// The user typed the detach keys; the process keeps running.
  Detached {
    process: String,
  },
}

/// What a relay forwards.
#[derive(Default)]
pub struct Options {
  /// The local terminal is raw for the duration.
  pub tty: bool,
  /// Forward local stdin.
  pub stdin: bool,
  /// Read stdin only for the detach keys: a read-only viewer.
  pub read_only: bool,
  /// Leave the process when these are typed; `None` never leaves.
  pub detach_keys: Option<DetachKeys>,
}

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

/// Bridges the local terminal or pipes to a remote process until it exits
/// or the user types the detach keys. With `tty`, the local terminal is raw
/// for the duration. A stream that ends before the process exited fails,
/// [`Error::ended`]. A second termination signal ends the relay with
/// [`Error::Terminated`]; the terminal is restored on every way out.
pub async fn run(mut stream: ExecStream, options: Options) -> Result<Ended, Error> {
  let raw = if options.tty { RawMode::enable() } else { None };
  let mut forward = Forward::start_with(stream.input(), &options);
  let mut modes = terminal::Modes::default();
  let mut stdout = tokio::io::stdout();
  let mut stderr = tokio::io::stderr();
  // Set once the detach keys were typed: the relay waits for the executor
  // to take the detach, so the process is not hung up as the client leaves.
  let mut leaving: Option<tokio::time::Instant> = None;
  let mut process = String::new();
  let exit = loop {
    let deadline = leaving.unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(3600));
    let next = tokio::select! {
      next = stream.next() => next,
      Some(signal) = forward.terminated.recv() => return Err(Error::Terminated(signal)),
      Some(()) = forward.detached.recv(), if leaving.is_none() => {
        // NOTICE: an executor that does not answer within 2 s (an older
        // build, or a lost connection) is left anyway; a process it did not
        // see detach from then gets hung up.
        leaving = Some(tokio::time::Instant::now() + Duration::from_secs(2));
        continue;
      }
      () = tokio::time::sleep_until(deadline), if leaving.is_some() => break Ended::Detached { process },
    };
    let next = match next {
      Err(e) if leaving.is_some() => {
        tracing::debug!("relay: the stream ended while detaching: {e}");
        break Ended::Detached { process };
      }
      next => next?,
    };
    let Some(event) = next else {
      if leaving.is_some() {
        break Ended::Detached { process };
      }
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
      Event::Exited(exit) => break Ended::Exited(exit),
      Event::Detached => break Ended::Detached { process },
      // A person at a terminal sees the command's output, not the trace
      // events of its placement.
      Event::Started(started) => process = started.process_id,
      Event::Trace(_) => {}
    }
  };
  drop(raw);
  Ok(exit)
}

/// Forwards local stdin, signals and, with `tty`, window changes to a remote
/// process. The forwarding tasks stop when this value is dropped.
pub struct Forward {
  // NOTICE: the tasks are cancelled, not awaited.
  _stop: DropGuard,
  /// Ends the thread that reads stdin, without reading more of it (see
  /// [`stdin`]).
  _stdin: Option<stdin::Stop>,
  /// The name of a second termination signal, after which the client
  /// leaves.
  pub terminated: mpsc::Receiver<&'static str>,
  /// The detach keys were typed and the detach request sent.
  pub detached: mpsc::Receiver<()>,
}

impl Forward {
  pub fn start(input: Input, stdin: bool, tty: bool) -> Self {
    let options = Options {
      tty,
      stdin,
      ..Options::default()
    };
    Self::start_with(input, &options)
  }

  fn start_with(input: Input, options: &Options) -> Self {
    let cancel = CancellationToken::new();
    let (detached_tx, detached) = mpsc::channel(1);
    let mut stop_stdin = None;
    if options.stdin || options.detach_keys.is_some() {
      let stdin = Stdin {
        forward: options.stdin && !options.read_only,
        keys: options.detach_keys.clone(),
        detached: detached_tx,
      };
      // A stdin that cannot be read ends at once, as a read error does.
      let chunks = match stdin::read() {
        Ok((chunks, stop)) => {
          stop_stdin = Some(stop);
          chunks
        }
        Err(e) => {
          tracing::debug!("relay: cannot read stdin: {e}");
          let (ended, chunks) = mpsc::channel(1);
          // A new channel has room for its one chunk.
          let _ = ended.try_send(Vec::new());
          chunks
        }
      };
      tokio::spawn(forward_stdin(input.clone(), chunks, stdin, cancel.clone()));
    }
    let (terminated_tx, terminated) = mpsc::channel(1);
    // A read-only viewer sends nothing; its window size would be refused.
    if !options.read_only {
      tokio::spawn(signals::forward(input, options.tty, terminated_tx, cancel.clone()));
    }
    Self {
      _stop: cancel.drop_guard(),
      _stdin: stop_stdin,
      terminated,
      detached,
    }
  }
}

/// What the stdin task does with what it reads.
struct Stdin {
  /// Send it to the process; otherwise only look for the detach keys.
  forward: bool,
  keys: Option<DetachKeys>,
  detached: mpsc::Sender<()>,
}

/// Forwards local stdin to the process, then half-closes it at end of file.
/// The detach keys are taken out; when they are typed, it asks the
/// executor to detach and stops.
async fn forward_stdin(input: Input, mut chunks: stdin::Chunks, mut stdin_task: Stdin, cancel: CancellationToken) {
  loop {
    let read = tokio::select! {
      read = chunks.recv() => read,
      () = cancel.cancelled() => return,
    };
    // The thread ends without a chunk only when the relay stopped it.
    let Some(read) = read else { return };
    let n = read.len();
    let (bytes, detach) = match stdin_task.keys.as_mut() {
      Some(keys) if n > 0 => {
        let scanned = keys.scan(&read);
        (scanned.forward, scanned.detach)
      }
      _ => (read, false),
    };
    if !stdin_task.forward {
      if detach {
        leave(&input, &stdin_task.detached).await;
      }
      if detach || n == 0 {
        return;
      }
      continue;
    }
    // A closed stream has nobody to tell; end of file half-closes stdin.
    let sent = if n == 0 {
      input.eof().await
    } else if bytes.is_empty() {
      Ok(())
    } else {
      input.stdin(bytes).await
    };
    if detach {
      leave(&input, &stdin_task.detached).await;
      return;
    }
    if sent.is_err() || n == 0 {
      return;
    }
  }
}

/// Asks the executor to detach this client and tells the relay.
async fn leave(input: &Input, detached: &mpsc::Sender<()>) {
  if let Err(e) = input.detach().await {
    tracing::debug!("relay: cannot send the detach: {e}");
  }
  // The relay is waiting for this; a full queue already said it.
  if detached.try_send(()).is_err() {
    tracing::debug!("relay: leaving already");
  }
}

// The tests serve the executor on a Unix socket.
#[cfg(all(test, unix))]
#[path = "mod_test.rs"]
mod tests;
