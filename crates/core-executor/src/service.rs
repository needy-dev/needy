//! `ExecutorService`: the process service of one machine, over its
//! [`process::Host`](crate::process::Host).

use std::pin::Pin;
use std::time::Duration;

use futures_util::{Stream, StreamExt as _};
use needy_api_proto::v1::process_service_server::ProcessService;
use needy_api_proto::v1::{
  ExecExited, ExecRequest, ExecResponse, ExecStarted, KillProcessRequest, KillProcessResponse, ListProcessesRequest, ListProcessesResponse,
  ProcessInfo, TermFallback, exec_request, exec_response,
};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::process::{Client, Error, Exit, Host, Input, Output, Proc, Spec, Subscriber, Subscription};

pub type ExecStream = Pin<Box<dyn Stream<Item = Result<ExecResponse, Status>> + Send>>;

/// A client's exec requests after the first: from a gRPC stream, or from a
/// runner's task stream.
pub type Inbound = Pin<Box<dyn Stream<Item = Result<ExecRequest, Status>> + Send>>;

#[derive(Clone)]
pub struct ExecutorService {
  pub host: Host,
  pub name: String,
}

fn exited(exit: &Exit) -> ExecResponse {
  ExecResponse {
    diagnostic: None,
    payload: Some(exec_response::Payload::Exited(ExecExited {
      code: exit.code,
      signal: exit.signal.clone(),
    })),
  }
}

/// Streams a process to one client, from `subscription`, until it exits or
/// the client leaves. A client that leaves a process it did not detach takes
/// the process down; `detach` is set when the process started detached or
/// the client sent a `detach` request. The stream's tasks belong to `host`.
fn stream_process(
  host: &Host,
  proc: Arc<Proc>,
  client: Client,
  mut inbound: Inbound,
  started: ExecStarted,
  subscription: Subscription,
  detach: bool,
) -> ExecStream {
  let (tasks, cancel) = host.tasks();
  let (tx, rx) = mpsc::channel::<Result<ExecResponse, Status>>(64);
  let input_proc = proc.clone();
  let input_cancel = cancel.clone();
  let detach = Arc::new(AtomicBool::new(detach));
  let detach_input = detach.clone();
  let ack = tx.clone();
  tasks.spawn(async move {
    // The client leaves the process's seats when its input ends.
    let client = client;
    loop {
      let message = tokio::select! {
        message = inbound.next() => message,
        () = input_cancel.cancelled() => break,
      };
      let payload = match message {
        Some(Ok(message)) => message.payload,
        None => break,
        Some(Err(status)) => {
          tracing::debug!("process {}: client stream: {status}", input_proc.id);
          break;
        }
      };
      match payload {
        Some(exec_request::Payload::Stdin(bytes)) => client.send(Input::Stdin(bytes)),
        Some(exec_request::Payload::StdinEof(_)) => client.send(Input::Eof),
        Some(exec_request::Payload::Signal(sig)) => client.send(Input::Signal(sig)),
        Some(exec_request::Payload::Resize(size)) => client.send(Input::Resize(clamp(size.rows), clamp(size.cols))),
        Some(exec_request::Payload::Detach(_)) => {
          detach_input.store(true, Ordering::SeqCst);
          let detached = ExecResponse {
            diagnostic: None,
            payload: Some(exec_response::Payload::Detached(true)),
          };
          // The client waits for this before it leaves; one that left
          // already needs no answer.
          if ack.send(Ok(detached)).await.is_err() {
            tracing::debug!("process {}: the client left before its detach was answered", input_proc.id);
          }
        }
        // A second start or attach on one stream has no meaning.
        Some(exec_request::Payload::Start(_) | exec_request::Payload::Attach(_)) | None => {}
      }
    }
  });
  let cancel = cancel.clone();
  tasks.spawn(async move {
    tokio::select! {
      () = relay_output(proc, tx, started, subscription, detach) => {}
      // The host kills its processes when it stops; the client sees the
      // stream end.
      () = cancel.cancelled() => {}
    }
  });
  Box::pin(ReceiverStream::new(rx))
}

async fn relay_output(
  proc: Arc<Proc>,
  tx: mpsc::Sender<Result<ExecResponse, Status>>,
  started: ExecStarted,
  mut subscription: Subscription,
  detach: Arc<AtomicBool>,
) {
  let send = |event| {
    let tx = tx.clone();
    async move { tx.send(Ok(event)).await.is_ok() }
  };
  let mut alive = send(ExecResponse {
    diagnostic: None,
    payload: Some(exec_response::Payload::Started(started)),
  })
  .await;
  // The replay is there at once: one batch, or several when it is large.
  let mut past = Batch::default();
  for event in std::mem::take(&mut subscription.past) {
    past.push(&event);
  }
  for response in past.responses {
    if !alive {
      break;
    }
    let done = matches!(response.payload, Some(exec_response::Payload::Exited(_)));
    alive = send(response).await;
    if done {
      return;
    }
  }
  // The first output after a quiet `BATCH_INTERVAL` goes at once, so a
  // keystroke's echo is not delayed; output that follows waits for the rest
  // of the interval and goes with it.
  let mut sent_at: Option<Instant> = None;
  while alive {
    // A client that leaves closes its stream; a process that writes nothing
    // more is hung up then, not at its next output.
    let next = tokio::select! {
      next = subscription.live.recv() => next,
      () = tx.closed() => {
        alive = false;
        break;
      }
    };
    let Some(event) = next else {
      fell_behind(&proc, &tx, &subscription).await;
      break;
    };
    let mut batch = Batch::default();
    batch.push(&event);
    let deadline = sent_at.map_or_else(Instant::now, |at| at + BATCH_INTERVAL);
    let mut closed = false;
    while !batch.exited && batch.bytes < BATCH_BYTES {
      // Biased: output already queued joins the batch even when the
      // deadline has passed.
      tokio::select! {
        biased;
        event = subscription.live.recv() => match event {
          Some(event) => batch.push(&event),
          None => {
            closed = true;
            break;
          }
        },
        () = tokio::time::sleep_until(deadline) => break,
      }
    }
    sent_at = Some(Instant::now());
    for response in batch.responses {
      alive = send(response).await;
      if !alive {
        break;
      }
    }
    if batch.exited {
      return;
    }
    if closed {
      if alive {
        fell_behind(&proc, &tx, &subscription).await;
      }
      break;
    }
  }
  if !alive && !detach.load(Ordering::SeqCst) && proc.exit().is_none() {
    let sig = if proc.tty { "SIGHUP" } else { "SIGTERM" };
    proc.send(Input::Signal(sig.into()));
  }
}

/// Tells a client whose subscription ended without an exit that it fell
/// behind, when it did.
async fn fell_behind(proc: &Proc, tx: &mpsc::Sender<Result<ExecResponse, Status>>, subscription: &Subscription) {
  if subscription.fell_behind() {
    // The client is told why its stream ends without an exit.
    let lagged = Status::resource_exhausted("the client fell behind the process's output; attach again");
    if tx.send(Err(lagged)).await.is_err() {
      tracing::debug!("process {}: the client left as it fell behind", proc.id);
    }
  }
}

// Source: https://github.com/hyperium/h2/blob/b0e890842ac16d3c76836fd4fcc06421acb15fa7/src/proto/connection.rs#L99-L109
// (h2 v0.4.20, the budget: half the target connection window, at least
// 25600), https://github.com/hyperium/h2/blob/b0e890842ac16d3c76836fd4fcc06421acb15fa7/src/proto/streams/counts.rs#L120-L138
// (a frame under 256 bytes costs 256 minus its length until it is read),
// https://github.com/hyperium/hyper/blob/7f67699ce32d717081deb7018b4da7d679277ac0/src/proto/h2/server.rs#L36
// (hyper v1.12.0, a server's 1 MiB connection window).
// Reference: https://github.com/hyperium/h2/blob/b0e890842ac16d3c76836fd4fcc06421acb15fa7/CHANGELOG.md
// (0.4.16 "Fix limiting excessive amount of small DATA frames").
// NOTICE: a process's output goes to its client at most once per
// `BATCH_INTERVAL`, the chunks of one stream merged. Each message is an
// HTTP/2 DATA frame, and h2 ends a connection with GOAWAY ENHANCE_YOUR_CALM
// `too_many_data_frames` when its peer leaves too many frames under 256
// bytes unread: about 2,000 frames of a few bytes on a control plane's
// connection. A program that writes in small pieces, such as PowerShell
// writing each number and its line end on its own, sent tens of thousands
// of messages a second, and a control plane or a client that read more
// slowly than that lost the connection with every stream on it. 10 ms keeps
// a terminal responsive (under a 60 Hz frame) and at most 100 messages a
// second per stream.
// TODO(h2-budget): a reader that stops reading (a paused pager) while a
// program keeps writing small pieces still fills the budget, after about
// 20 s at the full rate. hyper 1.12 does not expose h2's `data_frame_budget`;
// raising the connection windows raises the budget too. Revisit when a
// stalled client is seen to lose its connection.
const BATCH_INTERVAL: Duration = Duration::from_millis(10);

// NOTICE: a batch is sent once it holds this much without waiting for the
// interval; one merged chunk stays under it too. The size of one read from
// a process (`process::pipes`), well under gRPC's 4 MiB default message
// limit on the receiving side.
const BATCH_BYTES: usize = 256 * 1024;

/// Responses waiting to be sent: consecutive output of one stream merged
/// into one chunk.
#[derive(Default)]
struct Batch {
  responses: Vec<ExecResponse>,
  bytes: usize,
  exited: bool,
}

impl Batch {
  fn push(&mut self, event: &Output) {
    let (bytes, stderr) = match event {
      Output::Stdout(bytes) => (bytes, false),
      Output::Stderr(bytes) => (bytes, true),
      Output::Exit(_) => {
        self.exited = true;
        self.responses.push(to_response(event));
        return;
      }
    };
    self.bytes += bytes.len();
    let last = match self.responses.last_mut().and_then(|response| response.payload.as_mut()) {
      Some(exec_response::Payload::Stdout(last)) if !stderr => Some(last),
      Some(exec_response::Payload::Stderr(last)) if stderr => Some(last),
      _ => None,
    };
    match last {
      Some(last) if last.len() + bytes.len() <= BATCH_BYTES => last.extend_from_slice(bytes),
      _ => self.responses.push(to_response(event)),
    }
  }
}

/// A window dimension as a terminal takes it: at least 1, at most `u16`.
fn clamp(n: u32) -> u16 {
  u16::try_from(n.max(1)).unwrap_or(u16::MAX)
}

/// A command that cannot start is a failed precondition, which clients
/// report like a shell does (127).
fn spawn_status(error: Error) -> Status {
  let (reason, facts) = match &error {
    Error::WorkingDirectory { program, cwd, .. } => ("process.cwd_invalid", vec![("program", program.clone()), ("cwd", cwd.clone())]),
    Error::Start { program, .. } => ("process.start_failed", vec![("program", program.clone())]),
    Error::StartTerminal { program, .. } => ("process.terminal_failed", vec![("program", program.clone())]),
    Error::Terminal(_) => ("process.terminal_failed", Vec::new()),
    Error::EmptyCommand => ("process.start_failed", Vec::new()),
    Error::ShuttingDown => return Status::unavailable(error.to_string()),
  };
  let status = Status::failed_precondition(error.to_string());
  let diagnostic = needy_api_proto::v1::ErrorDiagnostic {
    reason: reason.into(),
    facts: facts.into_iter().map(|(key, value)| (key.into(), value)).collect(),
  };
  needy_api_client::diagnostic::with_status(status, diagnostic)
}

fn to_response(event: &Output) -> ExecResponse {
  match event {
    Output::Stdout(b) => ExecResponse {
      diagnostic: None,
      payload: Some(exec_response::Payload::Stdout(b.to_vec())),
    },
    Output::Stderr(b) => ExecResponse {
      diagnostic: None,
      payload: Some(exec_response::Payload::Stderr(b.to_vec())),
    },
    Output::Exit(exit) => exited(exit),
  }
}

fn info(proc: &Proc, executor: &str) -> ProcessInfo {
  let exit = proc.exit();
  ProcessInfo {
    process_id: proc.id.clone(),
    argv: proc.argv.clone(),
    executor: executor.to_string(),
    pid: proc.pid,
    running: exit.is_none(),
    exited: exit.map(|e| ExecExited {
      code: e.code,
      signal: e.signal,
    }),
    tty: proc.tty,
    started_at_ms: proc.started_at_ms,
    run_id: proc.run_id.clone(),
    // TODO(process-origin): the executor does not keep who started a
    // process; the control daemon, which relays every start, fills it in.
    // Keeping it here waits for an executor that serves more than one daemon.
    origin: needy_api_proto::v1::Origin::Unspecified.into(),
  }
}

impl ExecutorService {
  /// Starts or attaches to a process for the client whose requests are
  /// `inbound`, the first of which starts or attaches, and returns the
  /// process's stream to that client. `exec` serves this over gRPC; a
  /// runner serves it over the task streams it opens to its control plane.
  pub async fn open(&self, mut inbound: Inbound) -> Result<ExecStream, Status> {
    let first = inbound.next().await.transpose()?.ok_or_else(|| Status::invalid_argument("empty request stream"))?;
    match first.payload {
      Some(exec_request::Payload::Start(start)) => {
        let tty = start.tty.map(|t| (clamp(t.rows), clamp(t.cols)));
        let spec = Spec {
          argv: start.argv,
          cwd: start.cwd,
          env: start.env.into_iter().collect(),
          clear_env: start.clear_env,
          tty,
          no_stdin: start.no_stdin,
          run_id: start.run_id,
          search_path_if_missing: start.search_path_if_missing,
        };
        let proc = self.host.spawn(spec).map_err(spawn_status)?;
        // The starting client subscribes before anything else runs, with the
        // replay buffer, so output written since the spawn reaches it too.
        // NOTICE: a process that writes more than the replay buffer (256
        // KiB) in that moment would lose its oldest output; subscribing
        // inside `Host::spawn`, before the output pumps start, would close
        // that gap if it is ever observed.
        let subscription = proc.subscribe(true, Subscriber::Owner);
        // A client that starts a process detached leaves at once; it joins
        // without a terminal, so it never stands for one that would answer
        // the program's terminal queries.
        let client = proc.join(tty.filter(|_| !start.detach), false);
        let started = ExecStarted {
          process_id: proc.id.clone(),
          executor: self.name.clone(),
          pid: proc.pid,
          argv: proc.argv.clone(),
          cwd: String::new(),
          term_fallback: proc.term_fallback.as_ref().map(|f| TermFallback {
            requested: f.requested.clone(),
            used: f.used.clone(),
          }),
        };
        Ok(stream_process(&self.host, proc, client, inbound, started, subscription, start.detach))
      }
      Some(exec_request::Payload::Attach(attach)) => {
        let proc = self.host.get(&attach.process_id).ok_or_else(|| Status::not_found("no such process"))?;
        let started = ExecStarted {
          process_id: proc.id.clone(),
          executor: self.name.clone(),
          pid: proc.pid,
          argv: proc.argv.clone(),
          cwd: String::new(),
          term_fallback: proc.term_fallback.as_ref().map(|f| TermFallback {
            requested: f.requested.clone(),
            used: f.used.clone(),
          }),
        };
        let subscription = proc.subscribe(attach.replay, Subscriber::Viewer);
        // Joined after subscribing: the attaching client owns the size, and
        // the redraw a full-screen program makes for it reaches this client.
        let window = attach.window.filter(|_| proc.tty).map(|w| (clamp(w.rows), clamp(w.cols)));
        let client = proc.join(window, attach.read_only);
        // An attached client never owns the process.
        Ok(stream_process(&self.host, proc, client, inbound, started, subscription, true))
      }
      _ => Err(Status::invalid_argument("the first request must start or attach")),
    }
  }
}

#[tonic::async_trait]
impl ProcessService for ExecutorService {
  type ExecStream = ExecStream;

  async fn exec(&self, request: Request<Streaming<ExecRequest>>) -> Result<Response<Self::ExecStream>, Status> {
    Ok(Response::new(self.open(Box::pin(request.into_inner())).await?))
  }

  async fn list_processes(&self, request: Request<ListProcessesRequest>) -> Result<Response<ListProcessesResponse>, Status> {
    let run = request.into_inner().run_id;
    let processes = self.host.list().iter().filter(|p| run.is_empty() || p.run_id == run).map(|p| info(p, &self.name)).collect();
    Ok(Response::new(ListProcessesResponse { processes }))
  }

  async fn kill_process(&self, request: Request<KillProcessRequest>) -> Result<Response<KillProcessResponse>, Status> {
    let request = request.into_inner();
    let proc = self.host.get(&request.process_id).ok_or_else(|| Status::not_found("no such process"))?;
    proc.send(Input::Signal(if request.signal.is_empty() {
      "SIGTERM".into()
    } else {
      request.signal
    }));
    Ok(Response::new(KillProcessResponse {}))
  }
}

#[cfg(all(test, unix))]
#[path = "service_test.rs"]
mod tests;
