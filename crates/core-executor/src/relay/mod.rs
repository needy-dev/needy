//! The client side of an exec stream: relays a process to the local terminal
//! or pipes and returns its exit status.
//!
//! Output formats for tools (`needy exec --output json|ndjson`) live in the
//! CLI; this module only relays a process to local stdio. [`terminal`] owns
//! raw mode and the window size, [`signals`] forwards local signals and
//! window changes.

mod signals;
mod terminal;

use needy_api_client::Client;
use needy_api_proto::v1::{ExecRequest, ExecResponse, exec_request, exec_response};
use needy_common::process::Exit;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::{CancellationToken, DropGuard};

use terminal::RawMode;
pub use terminal::{interactive, window};

#[derive(Debug, thiserror::Error)]
pub enum Error {
  /// The daemon refused or ended the stream. The status is not a `source`,
  /// so a chained report shows its message once.
  #[error("{}", .0.message())]
  Status(tonic::Status),
  /// The executor reported an error instead of an exit.
  #[error("{0}")]
  Remote(String),
  #[error("cannot write the process's output")]
  Output(#[from] std::io::Error),
}

impl From<tonic::Status> for Error {
  fn from(status: tonic::Status) -> Self {
    Self::Status(status)
  }
}

/// Opens an exec stream whose first request is `first`. The error is the
/// daemon's status, so callers can tell a refused command from a failure.
pub async fn open(
  client: &Client,
  first: ExecRequest,
) -> Result<(mpsc::Sender<ExecRequest>, tonic::Streaming<ExecResponse>), tonic::Status> {
  let (tx, rx) = mpsc::channel::<ExecRequest>(64);
  // The receiver is alive here, so the send only fails if the channel is
  // full, which a new channel is not.
  tx.send(first).await.map_err(|_| tonic::Status::internal("exec request channel closed"))?;
  let responses = client.processes().exec(ReceiverStream::new(rx)).await?.into_inner();
  Ok((tx, responses))
}

/// Opens an exec stream and relays it to local stdio until the process exits.
pub async fn run(client: &Client, first: ExecRequest, tty: bool, stdin: bool) -> Result<Exit, Error> {
  let (tx, responses) = open(client, first).await?;
  stdio(tx, responses, tty, stdin).await
}

/// Bridges the local terminal or pipes to a remote process until it exits.
/// With `tty`, the local terminal is raw for the duration.
pub async fn stdio(
  tx: mpsc::Sender<ExecRequest>,
  mut responses: tonic::Streaming<ExecResponse>,
  tty: bool,
  stdin: bool,
) -> Result<Exit, Error> {
  let raw = if tty { RawMode::enable() } else { None };
  let _forward = Forward::start(tx, stdin, tty);
  let mut stdout = tokio::io::stdout();
  let mut stderr = tokio::io::stderr();
  // TODO(fix): a stream that ends without an exit reports exit 0, as it
  // always has; it should be an error. Needs its own fix commit with a
  // regression test.
  let mut exit = Exit {
    code: Some(0),
    signal: String::new(),
  };
  while let Some(message) = responses.message().await? {
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
      Some(exec_response::Payload::Error(error)) => return Err(Error::Remote(error)),
      // A person at a terminal sees the command's output, not the trace
      // events of its placement.
      Some(exec_response::Payload::Started(_)) | Some(exec_response::Payload::Event(_)) | None => {}
    }
  }
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
}

impl Forward {
  pub fn start(tx: mpsc::Sender<ExecRequest>, stdin: bool, tty: bool) -> Self {
    let cancel = CancellationToken::new();
    if stdin {
      tokio::spawn(forward_stdin(tx.clone(), cancel.clone()));
    }
    tokio::spawn(signals::forward(tx, tty, cancel.clone()));
    Self {
      _stop: cancel.drop_guard(),
    }
  }
}

/// Forwards local stdin to the process, then half-closes it at end of file.
async fn forward_stdin(tx: mpsc::Sender<ExecRequest>, cancel: CancellationToken) {
  let mut stdin = tokio::io::stdin();
  let mut buf = vec![0u8; 32 * 1024];
  loop {
    let read = tokio::select! {
      read = stdin.read(&mut buf) => read,
      () = cancel.cancelled() => return,
    };
    let payload = match read {
      Ok(0) => exec_request::Payload::StdinEof(true),
      Ok(n) => exec_request::Payload::Stdin(buf[..n].to_vec()),
      Err(e) => {
        tracing::debug!("relay: stdin: {e}");
        exec_request::Payload::StdinEof(true)
      }
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
      return;
    }
  }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
