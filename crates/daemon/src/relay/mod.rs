//! The exec relay, through `api-server`'s `Processes` trait: places a
//! command, takes a slot on its instance, opens the upstream exec stream
//! there, and relays both ways through the stream filters installed for the
//! session. A session's relays are its tasks and stop with it; a relay
//! outside any session is the daemon's.

mod filter;

use std::sync::Arc;
use std::time::Instant;

use needy_api_client::exec::{self as client_exec, ExecStream};
use needy_api_client::proto::v1::{KillProcessRequest, ListProcessesRequest};
use needy_api_server::control::{ControlError, ExecInput, ExecOpen, ExecOutput, Inputs, Outputs, ProcessInfo, Processes};
use needy_api_server::protocol::domain;
use needy_common::time::now_ms;
use needy_core::event::{Event, ExecExit, ExecStart};
use needy_core::intercept::StreamFilter;
use needy_core::types::Origin;
use needy_trace::Dispatch;
use tokio::sync::mpsc;
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::ReceiverStream;

use crate::Daemon;
use crate::pool::{Instance, Slot};
use crate::routing;
use crate::session::{self, Session};

type Sender = mpsc::Sender<Result<ExecOutput, ControlError>>;

/// An executor's error, relayed with the same meaning.
fn relayed(e: needy_api_client::Error) -> ControlError {
  use needy_api_client::Kind;
  let message = e.to_string();
  match e.kind() {
    Kind::Denied => ControlError::denied(message),
    // The executor refused the daemon's own token; to the daemon's caller
    // the executor is unavailable, not the command refused.
    Kind::Unauthenticated => ControlError::Unavailable(message),
    Kind::NotFound => ControlError::NotFound(message),
    Kind::Invalid => ControlError::Invalid(message),
    Kind::Failed => ControlError::Failed(message),
    Kind::Exhausted => ControlError::Exhausted(message),
    // The executor's stream ended before the process exited.
    Kind::Unavailable | Kind::Ended => ControlError::Unavailable(message),
    Kind::Other => ControlError::Internal(message),
  }
}

/// A process the daemon has seen: where it runs, and who started it.
// NOTICE: executors do not keep a process's origin, so the daemon that
// relayed the start is the only one that knows it. A process this daemon
// only listed, such as one on a remote executor another daemon started,
// has no origin.
#[derive(Clone)]
pub(crate) struct Known {
  instance: Arc<Instance>,
  origin: Option<Origin>,
}

/// What the relay asks the instance for.
enum Upstream {
  Start(needy_api_server::control::ExecSpec),
  Attach { process: String, replay: bool },
}

/// One relayed stream's output side.
struct Relay {
  instance: Arc<Instance>,
  /// The session's trace; an exec without a session has no profile, so no
  /// chaos and no trace.
  trace: Option<Dispatch>,
  /// `None` for an attach, which records nothing.
  origin: Option<Origin>,
  argv: Vec<String>,
  detach: bool,
  filter: Option<Arc<dyn StreamFilter>>,
  requested: Instant,
}

impl Daemon {
  /// The instance a command outside any session runs on: this machine, or
  /// the one pooled instance of executor `name`.
  async fn sessionless(&self, name: &str) -> Result<Arc<Instance>, ControlError> {
    match name {
      "" | "local" => Ok(self.local.clone()),
      name => self.pool.named(name).await,
    }
  }

  fn process(&self, process: &str) -> Result<Known, ControlError> {
    let processes = self.processes.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    processes.get(process).cloned().ok_or_else(|| ControlError::NotFound("no such process".into()))
  }

  fn process_instance(&self, process: &str) -> Result<Arc<Instance>, ControlError> {
    self.process(process).map(|known| known.instance)
  }

  /// What the daemon knows of `process`, which `instance` listed: what it
  /// recorded, or else `instance` and `origin`, which are then recorded.
  fn sighted(&self, process: &str, instance: &Arc<Instance>, origin: Option<Origin>) -> Known {
    let mut processes = self.processes.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    processes
      .entry(process.to_string())
      .or_insert_with(|| Known {
        instance: instance.clone(),
        origin,
      })
      .clone()
  }

  /// Records the instance of `process` and, when this daemon started it, its
  /// origin; a later sighting without an origin keeps the one recorded.
  fn remember(&self, process: &str, instance: &Arc<Instance>, origin: Option<Origin>) {
    let mut processes = self.processes.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let origin = origin.or_else(|| processes.get(process).and_then(|known| known.origin));
    processes.insert(
      process.to_string(),
      Known {
        instance: instance.clone(),
        origin,
      },
    );
  }
}

#[async_trait::async_trait]
impl Processes for Daemon {
  async fn exec(&self, open: ExecOpen, input: Inputs) -> Result<Outputs, ControlError> {
    let requested = Instant::now();
    let (session, relay, decided, upstream) = match open {
      ExecOpen::Start(mut spec) => {
        let origin = spec.origin;
        // The session comes from the start request, never from a lookup by
        // an empty ID.
        let session: Option<Arc<Session>> = match spec.session.as_str() {
          "" => None,
          id => Some(self.sessions.find_open(id)?),
        };
        let (instance, decided) = match &session {
          Some(session) => routing::place(session, &mut spec).await?,
          None => (self.sessionless(&spec.executor).await?, Vec::new()),
        };
        if spec.cwd.is_empty() && !instance.handle.workspace.is_empty() {
          spec.cwd = instance.handle.workspace.clone();
        }
        spec.session = session.as_ref().map(|s| s.id.clone()).unwrap_or_default();
        let relay = Relay {
          instance,
          trace: session.as_ref().map(|s| s.trace.clone()),
          origin: Some(origin),
          argv: spec.argv.clone(),
          detach: spec.detach,
          filter: session.as_ref().and_then(|s| filter::install(s, origin, &spec.cwd)),
          requested,
        };
        (session, relay, decided, Upstream::Start(spec))
      }
      ExecOpen::Attach { process, replay } => {
        let known = self.process(&process)?;
        // An attach forwards the caller's input: keystrokes in an SFTP or tar
        // stream corrupt it, and its output is binary.
        if let Some(origin) = known.origin.filter(|o| o.is_internal()) {
          return Err(ControlError::denied(format!("process {process} is needy's own (origin {origin}); it cannot be attached")));
        }
        let relay = Relay {
          instance: known.instance,
          trace: None,
          origin: None,
          argv: Vec::new(),
          detach: false,
          filter: None,
          requested,
        };
        (None, relay, Vec::new(), Upstream::Attach { process, replay })
      }
    };
    let slot = relay.instance.slot().await?;
    let (out_tx, out_rx) = mpsc::channel(64);
    for event in decided {
      out_tx.send(Ok(ExecOutput::Trace(event))).await.map_err(|_| ControlError::Internal("exec output channel closed".into()))?;
    }
    let client = &relay.instance.handle.client;
    let upstream = match upstream {
      Upstream::Start(spec) => ExecStream::start(client, domain::proto_exec_start(spec)).await,
      Upstream::Attach { process, replay } => ExecStream::attach(client, &process, replay).await,
    }
    .map_err(relayed)?;
    // A session's relays stop with the session; the others with the daemon.
    let (tasks, cancel) = match &session {
      Some(session) => (&session.tasks, &session.cancel),
      None => (&self.tasks, &self.cancel),
    };
    tasks.spawn(cancel.clone().run_until_cancelled_owned(forward_input(
      input,
      upstream.input(),
      relay.filter.clone(),
      relay.trace.clone(),
      out_tx.clone(),
    )));
    let daemon = self.clone();
    let cancel = cancel.clone();
    tasks.spawn(async move {
      let ended = out_tx.clone();
      if cancel.run_until_cancelled(relay.run(daemon.clone(), upstream, slot, out_tx)).await.is_none() {
        // The session stopped or the daemon is shutting down before the
        // process exited; the caller learns why before the stream closes.
        // NOTICE: `try_send`, so a caller that reads nothing cannot hold up
        // the stop, which waits for this task; it then sees the stream
        // close without the reason.
        if let Err(e) = ended.try_send(Ok(ExecOutput::Ended(daemon.end_reason().into()))) {
          tracing::debug!("exec relay: cannot say why the stream ends: {e}");
        }
      }
    });
    Ok(Box::pin(ReceiverStream::new(out_rx)))
  }

  async fn list(&self, session: &str) -> Result<Vec<ProcessInfo>, ControlError> {
    // An empty or unknown session lists every executor; the executors keep
    // only the session's processes.
    let instances: Vec<Arc<Instance>> = match self.sessions.find(session) {
      Ok(session) => session.executors().into_values().collect(),
      Err(_) => {
        let mut all = vec![self.local.clone()];
        all.extend(self.pool.instances().await);
        all
      }
    };
    let request = ListProcessesRequest {
      session_id: session.to_string(),
    };
    let mut processes = Vec::new();
    // Instances that share one executor, such as this machine's `local` and
    // every native executor, list the same processes; each executor is
    // asked once.
    let mut asked: Vec<(&'static str, String)> = Vec::new();
    for instance in instances {
      let executor = (instance.handle.runtime, instance.handle.id.clone());
      if asked.contains(&executor) {
        continue;
      }
      asked.push(executor);
      // An executor that does not answer has nothing to list.
      match instance.handle.client.processes().list_processes(request.clone()).await {
        Ok(list) => {
          for p in list.into_inner().processes {
            let mut p = domain::process_info(p);
            // A process is named by the instance it was started through;
            // one this daemon did not start, by the first that listed it.
            let known = self.sighted(&p.process, &instance, p.origin);
            p.executor = known.instance.name.clone();
            p.origin = known.origin;
            processes.push(p);
          }
        }
        Err(status) => tracing::debug!("executor {}: cannot list processes: {}", instance.name, status.message()),
      }
    }
    processes.sort_by_key(|p| p.started_ms);
    Ok(processes)
  }

  async fn kill(&self, process: &str, signal: &str) -> Result<(), ControlError> {
    let instance = self.process_instance(process)?;
    let request = KillProcessRequest {
      process_id: process.to_string(),
      signal: signal.to_string(),
    };
    instance.handle.client.processes().kill_process(request).await.map_err(|s| relayed(s.into()))?;
    Ok(())
  }
}

/// Relays the caller's input to the process, through the stream's filter.
async fn forward_input(
  mut input: Inputs,
  upstream: client_exec::Input,
  filter: Option<Arc<dyn StreamFilter>>,
  trace: Option<Dispatch>,
  out_tx: Sender,
) {
  while let Some(mut message) = input.next().await {
    if let (Some(filter), ExecInput::Stdin(bytes)) = (&filter, &message) {
      let filtered = filter.inbound(bytes);
      if let Some(trace) = &trace {
        for fired in filtered.fired {
          session::emit(trace, &Event::FaultInjected(fired));
        }
      }
      for reply in filtered.replies {
        // The client is gone; the input loop ends on its next read.
        if out_tx.send(Ok(ExecOutput::Stdout(reply))).await.is_err() {
          break;
        }
      }
      if filtered.forward.is_empty() {
        continue;
      }
      message = ExecInput::Stdin(filtered.forward);
    }
    let sent = match message {
      ExecInput::Stdin(bytes) => upstream.stdin(bytes).await,
      ExecInput::Eof => upstream.eof().await,
      ExecInput::Signal(name) => upstream.signal(&name).await,
      ExecInput::Resize(size) => upstream.resize(domain::proto_window(size)).await,
    };
    if sent.is_err() {
      break;
    }
  }
}

impl Relay {
  /// Relays the process's output to the caller until it exits. The slot is
  /// held until then: a detached process keeps it after the caller leaves,
  /// so the stream is drained without a caller.
  async fn run(self, daemon: Daemon, mut upstream: ExecStream, slot: Slot, out_tx: Sender) {
    let _slot = slot;
    let started_ms = now_ms();
    let mut process_id = String::new();
    let mut client_gone = false;
    loop {
      let output = match upstream.next().await {
        Ok(Some(event)) => event,
        Ok(None) => break,
        // The executor reported an error instead of an exit; the stream
        // goes on.
        Err(needy_api_client::Error::Remote(message)) => {
          if !client_gone && out_tx.send(Ok(ExecOutput::Error(message))).await.is_err() {
            if !self.detach {
              break;
            }
            client_gone = true;
          }
          continue;
        }
        Err(e) => {
          if out_tx.send(Err(relayed(e))).await.is_err() {
            tracing::debug!("exec relay: the client left before the upstream error");
          }
          break;
        }
      };
      let mut done = false;
      let output = match output {
        client_exec::Event::Started(started) => {
          process_id = started.process_id.clone();
          daemon.remember(&started.process_id, &self.instance, self.origin);
          if let (Some(origin), Some(trace)) = (self.origin, &self.trace) {
            // From the request to the process running, placement and a
            // wait for the executor's slot included.
            trace.time("exec.start", self.requested.elapsed());
            session::emit(
              trace,
              &Event::ExecStart(ExecStart {
                process: process_id.clone(),
                executor: self.instance.name.clone(),
                argv: self.argv.clone(),
                origin,
              }),
            );
          }
          ExecOutput::Started(domain::started(started))
        }
        client_exec::Event::Stdout(mut bytes) => {
          if let Some(filter) = &self.filter {
            bytes = filter.outbound(&bytes);
            if bytes.is_empty() {
              continue;
            }
          }
          ExecOutput::Stdout(bytes)
        }
        client_exec::Event::Stderr(bytes) => ExecOutput::Stderr(bytes),
        client_exec::Event::Exited(exit) => {
          if let (Some(origin), Some(trace)) = (self.origin, &self.trace) {
            session::emit(
              trace,
              &Event::ExecExit(ExecExit {
                process: process_id.clone(),
                executor: self.instance.name.clone(),
                argv: self.argv.clone(),
                origin,
                code: exit.code,
                signal: (!exit.signal.is_empty()).then(|| exit.signal.clone()),
                ms: (now_ms() - started_ms).max(0) as u64,
              }),
            );
          }
          done = true;
          ExecOutput::Exited(exit)
        }
        // Executors place nothing, so they send no trace events.
        client_exec::Event::Trace(_) => continue,
      };
      if done {
        if out_tx.send(Ok(output)).await.is_err() {
          tracing::debug!("exec relay: the client left before the exit");
        }
        break;
      }
      if !client_gone && out_tx.send(Ok(output)).await.is_err() {
        if !self.detach {
          break;
        }
        client_gone = true;
      }
    }
  }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
