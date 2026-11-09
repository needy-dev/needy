//! Exec streams: start a process or attach to one, send it input, read its
//! events, or run it to completion.

use needy_api_proto::v1::{
  ExecAttach, ExecEvent, ExecExited, ExecRefusal, ExecRequest, ExecResponse, ExecStart, ExecStarted, WindowSize, exec_request, exec_response,
};
use needy_common::process::Exit;
use prost::Message as _;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::{Client, Error, Result};

// NOTICE: 64 requests in flight keep a fast writer ahead of the network
// without buffering much; input chunks are at most 1 MiB.
const QUEUE: usize = 64;
const CHUNK: usize = 1 << 20;

/// One process's stream: its [`Input`] and its events. Dropping it closes
/// the input; a process that was not detached then ends.
pub struct ExecStream {
  input: Input,
  responses: tonic::Streaming<ExecResponse>,
}

/// Sends a process input, signals and window changes. Clones share the
/// stream. A send fails with [`Error::Closed`] once the stream has ended.
#[derive(Clone)]
pub struct Input(mpsc::Sender<ExecRequest>);

/// What a process's stream carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
  /// A trace event of the command's placement, sent before it starts:
  /// `payload` is the JSON of a `needy_core::event` kind.
  Trace(ExecEvent),
  Started(ExecStarted),
  Stdout(Vec<u8>),
  Stderr(Vec<u8>),
  Exited(Exit),
}

/// A process run to completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
  pub stdout: Vec<u8>,
  pub stderr: Vec<u8>,
  pub exit: Exit,
}

impl ExecStream {
  /// Starts a process. A refused start is an [`Error::Status`]: of kind
  /// [`Kind::Denied`](crate::Kind::Denied) when a rule or a fault refused it
  /// (see [`Error::refusal`]), [`Kind::Failed`](crate::Kind::Failed) when the
  /// command cannot start.
  pub async fn start(client: &Client, start: ExecStart) -> Result<Self> {
    Self::open(client, exec_request::Payload::Start(start)).await
  }

  /// Attaches to a running or finished process; with `replay`, its buffered
  /// output comes first.
  pub async fn attach(client: &Client, process_id: &str, replay: bool) -> Result<Self> {
    let attach = ExecAttach {
      process_id: process_id.to_string(),
      replay,
    };
    Self::open(client, exec_request::Payload::Attach(attach)).await
  }

  async fn open(client: &Client, first: exec_request::Payload) -> Result<Self> {
    let (tx, rx) = mpsc::channel::<ExecRequest>(QUEUE);
    let input = Input(tx);
    // The receiver is alive and the channel empty, so this cannot fail.
    input.send(first).await?;
    let responses = client.processes().exec(ReceiverStream::new(rx)).await?.into_inner();
    Ok(Self { input, responses })
  }

  pub fn input(&self) -> Input {
    self.input.clone()
  }

  /// The next event; `None` when the stream ends. An error the executor
  /// reports instead of an exit is [`Error::Remote`].
  pub async fn next(&mut self) -> Result<Option<Event>> {
    loop {
      let Some(message) = self.responses.message().await? else {
        return Ok(None);
      };
      let event = match message.payload {
        Some(exec_response::Payload::Event(event)) => Event::Trace(event),
        Some(exec_response::Payload::Started(started)) => Event::Started(started),
        Some(exec_response::Payload::Stdout(bytes)) => Event::Stdout(bytes),
        Some(exec_response::Payload::Stderr(bytes)) => Event::Stderr(bytes),
        Some(exec_response::Payload::Exited(exited)) => Event::Exited(exit(exited)),
        Some(exec_response::Payload::Error(message)) => return Err(Error::Remote(message)),
        // A message from a newer server with a payload this build does not
        // know carries nothing for it.
        None => continue,
      };
      return Ok(Some(event));
    }
  }

  /// Feeds `stdin`, then end of input, and collects the output until the
  /// process exits. A stream that ends first is [`Error::Ended`].
  pub async fn run(mut self, stdin: Option<Vec<u8>>) -> Result<Output> {
    let input = self.input();
    // The writer runs beside the reader: a process may fill its output
    // before it has read all of its input.
    let writer = tokio::spawn(async move {
      for chunk in stdin.as_deref().unwrap_or_default().chunks(CHUNK) {
        input.stdin(chunk.to_vec()).await?;
      }
      input.eof().await
    });
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let exit = loop {
      match self.next().await? {
        Some(Event::Stdout(bytes)) => stdout.extend(bytes),
        Some(Event::Stderr(bytes)) => stderr.extend(bytes),
        Some(Event::Exited(exit)) => break exit,
        Some(Event::Trace(_) | Event::Started(_)) => {}
        None => return Err(Error::Ended),
      }
    };
    // A closed input means the process exited before reading all of it;
    // its exit status says why.
    match writer.await {
      Ok(Ok(()) | Err(Error::Closed)) => {}
      Ok(Err(error)) => tracing::debug!("exec input: {error}"),
      Err(error) => tracing::debug!("exec input task: {error}"),
    }
    Ok(Output {
      stdout,
      stderr,
      exit,
    })
  }
}

impl Input {
  async fn send(&self, payload: exec_request::Payload) -> Result<()> {
    let request = ExecRequest {
      payload: Some(payload),
    };
    self.0.send(request).await.map_err(|_| Error::Closed)
  }

  pub async fn stdin(&self, bytes: Vec<u8>) -> Result<()> {
    self.send(exec_request::Payload::Stdin(bytes)).await
  }

  /// Closes the process's standard input.
  pub async fn eof(&self) -> Result<()> {
    self.send(exec_request::Payload::StdinEof(true)).await
  }

  /// Sends a signal by name, for example `SIGINT`.
  pub async fn signal(&self, name: &str) -> Result<()> {
    self.send(exec_request::Payload::Signal(name.to_string())).await
  }

  pub async fn resize(&self, size: WindowSize) -> Result<()> {
    self.send(exec_request::Payload::Resize(size)).await
  }
}

impl Error {
  /// The trace events of a refused start: the faults chaos injected and the
  /// placement decision. Empty for any other error.
  pub fn refusal(&self) -> Result<Vec<ExecEvent>, prost::DecodeError> {
    match self {
      Error::Status(status) if status.code() == tonic::Code::PermissionDenied => Ok(ExecRefusal::decode(status.details())?.events),
      _ => Ok(Vec::new()),
    }
  }
}

fn exit(exited: ExecExited) -> Exit {
  Exit {
    code: exited.code,
    signal: exited.signal,
  }
}
