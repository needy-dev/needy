//! The exec relay, through `api-server`'s `Processes` trait: places a
//! command, takes a slot on its instance, opens the upstream exec stream
//! there, and relays both ways through the stream filters installed for the
//! run. A run's relays are its tasks and stop with it; a relay
//! outside any run is the daemon's.

mod filter;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use needy_api_client::exec::{self as client_exec, ExecStream};
use needy_api_client::proto::v1::{ExecAttach, KillProcessRequest, ListProcessesRequest};
use needy_api_server::control::{ControlError, ExecInput, ExecOpen, ExecOutput, Inputs, Outputs, ProcessInfo, Processes};
use needy_api_server::protocol::domain;
use needy_common::time::now_ms;
use needy_core::event::{Event, ExecExit, ExecKill, ExecStart, TermFallback};
use needy_core::intercept::StreamFilter;
use needy_core::types::{Origin, Principal};
use needy_trace::Dispatch;
use tokio::sync::mpsc;
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::ReceiverStream;

use crate::pool::{Instance, Slot};
use crate::run::{self, Run};
use crate::{Daemon, actor};
use crate::{routing, store};

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
// relayed the start is the only one that knows it, in memory and, for a
// run's process, in `state.db`. A process this daemon only listed, such as
// one on a remote executor another daemon started, has no origin.
#[derive(Clone)]
pub(crate) struct Known {
  instance: Arc<Instance>,
  /// The executor it is listed under: the instance's name, or the
  /// executor `state.db` recorded for a run's process this daemon did not
  /// relay in this life, which a runner's machine lists.
  name: String,
  origin: Option<Origin>,
  /// The run whose trace records signals sent to it; `None` outside runs.
  run: Option<String>,
}

/// What the relay asks the instance for.
enum Upstream {
  Start(needy_api_server::control::ExecSpec),
  Attach {
    process: String,
    replay: bool,
    window: Option<needy_api_server::control::WindowSize>,
    read_only: bool,
  },
}

/// One relayed stream's output side.
struct Relay {
  instance: Arc<Instance>,
  /// The run's trace; an exec without a run has no profile, so no
  /// chaos and no trace.
  trace: Option<Dispatch>,
  /// `None` for an attach, which records nothing.
  origin: Option<Origin>,
  /// The run whose executor runs the process, which `state.db` records it
  /// under; `None` outside a run and for an attach.
  run: Option<String>,
  /// Who asked for the command; `None` for an attach and for the control
  /// plane's own.
  by: Option<Principal>,
  argv: Vec<String>,
  /// The process keeps running when the caller leaves: it started detached,
  /// or the caller sent `detach`.
  detach: Arc<AtomicBool>,
  filter: Option<Arc<dyn StreamFilter>>,
  requested: Instant,
}

impl Daemon {
  /// The instance where a command outside any run executes: the runner
  /// `runner`'s machine, this machine, or the one pooled instance of
  /// executor `name`.
  async fn outside_run(&self, name: &str, runner: &str) -> Result<Arc<Instance>, ControlError> {
    if !runner.is_empty() {
      if !name.is_empty() {
        return Err(ControlError::Invalid("name an executor or a runner, not both".into()));
      }
      return self.runner_instance(runner);
    }
    match name {
      "" | "local" => Ok(self.local.clone()),
      name => self.pool.named(name).await,
    }
  }

  /// A connected runner's machine; one that is not connected fails at once.
  fn runner_instance(&self, runner: &str) -> Result<Arc<Instance>, ControlError> {
    if self.links.get(runner).is_none() {
      return Err(ControlError::Unavailable(crate::link::not_connected(runner)));
    }
    Ok(Instance::runner(runner, &self.links))
  }

  /// The driving program of run `id`'s actor, on its terminal: the
  /// process the actor's process service runs.
  async fn driving_program(&self, id: &str) -> Result<(String, Known), ControlError> {
    let run = self.runs.find_open(id).await?;
    if run.tty.is_none() {
      return Err(ControlError::Failed(format!(
        "the driving program of run {} has no terminal; create the run with `needy run create -t`",
        run.id
      )));
    }
    let instance = run.terminal.get_or_try_init(|| actor::terminal(&run)).await?.clone();
    let listed = instance.handle.executor.list_processes(ListProcessesRequest::default()).await.map_err(|s| relayed(s.into()))?;
    let Some(process) = listed.processes.into_iter().find(|p| p.tty) else {
      return Err(ControlError::Unavailable(format!("the actor of run {} serves no terminal", run.id)));
    };
    let known = self.sighted(&process.process_id, &instance, None, Some(run.id.clone())).await;
    Ok((process.process_id, known))
  }

  fn process(&self, process: &str) -> Result<Known, ControlError> {
    let processes = self.processes.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    processes.get(process).cloned().ok_or_else(|| ControlError::NotFound("no such process".into()))
  }

  /// What the daemon knows of `process`, which `instance` listed: what it
  /// keeps in memory, or else what `state.db` recorded of a run's process
  /// (its executor and origin) or `instance` and `origin`, which are then
  /// kept.
  async fn sighted(&self, process: &str, instance: &Arc<Instance>, origin: Option<Origin>, run: Option<String>) -> Known {
    if let Ok(known) = self.process(process) {
      return known;
    }
    // TODO(process-origin): `state.db` records only runs' processes, so one
    // outside any run is listed without its origin after a restart.
    // Recording those too waits for the owner to want them in `state.db`.
    let recorded = match &run {
      Some(_) => self.recorded(process).await,
      None => None,
    };
    let (name, origin) = match recorded {
      Some((name, recorded)) => (name, recorded.or(origin)),
      None => (instance.name.clone(), origin),
    };
    let mut processes = self.processes.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    processes
      .entry(process.to_string())
      .or_insert_with(|| Known {
        instance: instance.clone(),
        name,
        origin,
        run,
      })
      .clone()
  }

  /// The executor and origin `state.db` recorded for a run's `process`.
  async fn recorded(&self, process: &str) -> Option<(String, Option<Origin>)> {
    let record = match self.state.process(process).await {
      Ok(record) => record?,
      Err(e) => {
        tracing::debug!("state.db: cannot read process {process}: {e:#}");
        return None;
      }
    };
    let instances = match self.state.instances_of(&record.run).await {
      Ok(instances) => instances,
      Err(e) => {
        tracing::debug!("state.db: cannot read the instances of run {}: {e:#}", record.run);
        return None;
      }
    };
    let name = instances.into_iter().find(|i| i.id == record.instance)?.name;
    Some((name, record.origin))
  }

  /// Records the instance of `process` and, when this daemon started it, its
  /// origin; a later sighting without an origin keeps the one recorded.
  fn remember(&self, process: &str, instance: &Arc<Instance>, origin: Option<Origin>, run: Option<String>) {
    let mut processes = self.processes.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let origin = origin.or_else(|| processes.get(process).and_then(|known| known.origin));
    let run = run.or_else(|| processes.get(process).and_then(|known| known.run.clone()));
    processes.insert(
      process.to_string(),
      Known {
        instance: instance.clone(),
        name: instance.name.clone(),
        origin,
        run,
      },
    );
  }
}

#[async_trait::async_trait]
impl Processes for Daemon {
  async fn exec(&self, open: ExecOpen, input: Inputs) -> Result<Outputs, ControlError> {
    let requested = Instant::now();
    let (run, relay, decided, upstream) = match open {
      ExecOpen::Start(mut spec) => {
        let origin = spec.origin;
        // The run comes from the start request, never from a lookup by
        // an empty ID.
        let run: Option<Arc<Run>> = match spec.run.as_str() {
          "" => None,
          _ if !spec.runner.is_empty() => {
            return Err(ControlError::Invalid("a command in a run is placed by the run's rules; name a run or a runner, not both".into()));
          }
          id => Some(self.runs.find_open(id).await?),
        };
        let (instance, decided) = match &run {
          Some(run) => routing::place(run, &mut spec).await?,
          None => (self.outside_run(&spec.executor, &spec.runner).await?, Vec::new()),
        };
        if spec.cwd.is_empty() && !instance.handle.workspace.is_empty() {
          spec.cwd = instance.handle.workspace.clone();
        }
        spec.run = run.as_ref().map(|s| s.id.clone()).unwrap_or_default();
        let relay = Relay {
          instance,
          trace: run.as_ref().map(|s| s.trace.clone()),
          origin: Some(origin),
          run: run.as_ref().map(|r| r.id.clone()),
          by: spec.by.clone(),
          argv: spec.argv.clone(),
          detach: Arc::new(AtomicBool::new(spec.detach)),
          filter: run.as_ref().and_then(|s| filter::install(s, origin, &spec.cwd)),
          requested,
        };
        (run, relay, decided, Upstream::Start(spec))
      }
      ExecOpen::Attach {
        process,
        run,
        replay,
        window,
        read_only,
      } => {
        let (process, known) = match (process.as_str(), run.as_str()) {
          ("", "") => return Err(ControlError::Invalid("name a process or a run".into())),
          ("", run) => self.driving_program(run).await?,
          (process, "") => (process.to_string(), self.process(process)?),
          _ => return Err(ControlError::Invalid("name a process or a run, not both".into())),
        };
        // An attach forwards the caller's input: keystrokes in an SFTP or tar
        // stream corrupt it, and its output is binary.
        if let Some(origin) = known.origin.filter(|o| o.is_internal()) {
          return Err(ControlError::denied(format!("process {process} is needy's own (origin {origin}); it cannot be attached")));
        }
        // An attach shares the process's run lifetime even though it adds
        // no second exec record to that run's trace or state.db.
        let owner = known.run.as_deref().and_then(|id| self.runs.exact(id));
        let relay = Relay {
          instance: known.instance,
          trace: None,
          origin: None,
          run: None,
          by: None,
          argv: Vec::new(),
          detach: Arc::new(AtomicBool::new(false)),
          filter: None,
          requested,
        };
        (
          owner,
          relay,
          Vec::new(),
          Upstream::Attach {
            process,
            replay,
            window,
            read_only,
          },
        )
      }
    };
    let slot = relay.instance.slot().await?;
    let (out_tx, out_rx) = mpsc::channel(64);
    for event in decided {
      out_tx.send(Ok(ExecOutput::Trace(event))).await.map_err(|_| ControlError::Internal("exec output channel closed".into()))?;
    }
    let executor = &*relay.instance.handle.executor;
    // A run's command that no path map maps keeps the actor's directory.
    let unmapped_cwd = match (&run, &upstream) {
      (Some(_), Upstream::Start(spec)) if routing::unmapped(&relay.instance, &spec.cwd) => Some(spec.cwd.clone()),
      _ => None,
    };
    let upstream = match upstream {
      Upstream::Start(spec) => ExecStream::start(executor, domain::proto_exec_start(spec)).await,
      Upstream::Attach {
        process,
        replay,
        window,
        read_only,
      } => {
        let attach = ExecAttach {
          process_id: process,
          replay,
          window: window.map(domain::proto_window),
          read_only,
          run_id: String::new(),
        };
        ExecStream::attach(executor, attach).await
      }
    }
    .map_err(relayed)
    .map_err(|error| match (error, unmapped_cwd) {
      (ControlError::Failed(message), Some(cwd)) if message.contains(&format!("working directory {cwd} ")) => ControlError::Failed(format!(
        "{message}; executor {} has no path_map entry for it, so the command kept the actor's directory",
        relay.instance.name
      )),
      (error, _) => error,
    })?;
    // A run's relays stop with the run; the others when the daemon begins
    // to shut down.
    let (tasks, cancel) = match &run {
      Some(run) => (&run.tasks, &run.cancel),
      None => (&self.tasks, &self.outside_runs),
    };
    tasks.spawn(cancel.clone().run_until_cancelled_owned(forward_input(
      input,
      upstream.input(),
      relay.filter.clone(),
      relay.trace.clone(),
      relay.detach.clone(),
      out_tx.clone(),
    )));
    let daemon = self.clone();
    let cancel = cancel.clone();
    tasks.spawn(async move {
      let ended = out_tx.clone();
      if cancel.run_until_cancelled(relay.run(daemon.clone(), upstream, slot, out_tx)).await.is_none() {
        // The run stopped or the daemon is shutting down before the
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

  async fn list(&self, run: &str, runner: &str) -> Result<Vec<ProcessInfo>, ControlError> {
    // An empty or unknown run lists every executor and connected runner;
    // the executors keep only the run's processes.
    let instances: Vec<Arc<Instance>> = if !runner.is_empty() {
      if !run.is_empty() {
        return Err(ControlError::Invalid("name a run or a runner, not both".into()));
      }
      vec![self.runner_instance(runner)?]
    } else {
      match self.runs.find(run) {
        Ok(run) => run.executors().into_values().collect(),
        Err(_) => {
          let mut all = vec![self.local.clone()];
          all.extend(self.pool.instances().await);
          all.extend(self.links.names().iter().map(|runner| Instance::runner(runner, &self.links)));
          all
        }
      }
    };
    let request = ListProcessesRequest {
      run_id: run.to_string(),
      ..Default::default()
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
      // An executor that does not answer has nothing to list, unless it
      // is the runner the caller named.
      match instance.handle.executor.list_processes(request.clone()).await {
        Ok(list) => {
          for p in list.processes {
            let mut p = domain::process_info(p);
            // A process is named by the instance it was started through;
            // one this daemon did not start, by the first that listed it.
            let known = self.sighted(&p.process, &instance, p.origin, Some(p.run.clone()).filter(|run| !run.is_empty())).await;
            p.executor = known.name.clone();
            p.origin = known.origin;
            processes.push(p);
          }
        }
        Err(status) if !runner.is_empty() => return Err(relayed(status.into())),
        Err(status) => tracing::debug!("executor {}: cannot list processes: {}", instance.name, status.message()),
      }
    }
    processes.sort_by_key(|p| p.started_ms);
    Ok(processes)
  }

  async fn kill(&self, process: &str, signal: &str, by: Principal) -> Result<(), ControlError> {
    let known = self.process(process)?;
    let request = KillProcessRequest {
      process_id: process.to_string(),
      signal: signal.to_string(),
    };
    known.instance.handle.executor.kill_process(request).await.map_err(|s| relayed(s.into()))?;
    // A run that ended has closed its trace; its processes were ended with
    // it.
    if let Some(run) = known.run.as_deref().and_then(|run| self.runs.exact(run)) {
      run.emit(&Event::ExecKill(ExecKill {
        process: process.to_string(),
        executor: known.name.clone(),
        signal: signal.to_string(),
        by: Some(by),
      }));
    }
    Ok(())
  }
}

/// Relays the caller's input to the process, through the stream's filter.
async fn forward_input(
  mut input: Inputs,
  upstream: client_exec::Input,
  filter: Option<Arc<dyn StreamFilter>>,
  trace: Option<Dispatch>,
  detach: Arc<AtomicBool>,
  out_tx: Sender,
) {
  while let Some(mut message) = input.next().await {
    if let (Some(filter), ExecInput::Stdin(bytes)) = (&filter, &message) {
      let filtered = filter.inbound(bytes);
      if let Some(trace) = &trace {
        for fired in filtered.fired {
          run::emit(trace, &Event::FaultInjected(fired));
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
      ExecInput::Detach => {
        // Set before the executor hears it, so the relay drains the stream
        // once the caller is gone instead of dropping it.
        detach.store(true, Ordering::SeqCst);
        upstream.detach().await
      }
    };
    if sent.is_err() {
      break;
    }
  }
}

impl Relay {
  /// Records in `state.db` that a run's process started.
  async fn record_start(&self, daemon: &Daemon, process: &str, started_ms: i64) {
    let Some(run) = &self.run else {
      return;
    };
    let record = store::ProcessRecord {
      id: process.to_string(),
      run: run.clone(),
      instance: store::executor_instance(run, &self.instance.name),
      origin: self.origin,
      state: store::ProcessState::Running,
      exit: None,
      started_ms,
      ended_ms: None,
    };
    if let Err(e) = daemon.state.save_process(&record).await {
      tracing::warn!("run {run}: cannot write process {process} to state.db: {e:#}");
    }
  }

  /// Records in `state.db` how a run's process exited.
  async fn record_exit(&self, daemon: &Daemon, process: &str, exit: &needy_common::process::Exit) {
    let Some(run) = &self.run else {
      return;
    };
    if let Err(e) = daemon.state.end_process(process, store::ProcessState::Exited, Some(exit.clone()), now_ms()).await {
      tracing::warn!("run {run}: cannot write the exit of process {process} to state.db: {e:#}");
    }
  }

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
            if !self.detach.load(Ordering::SeqCst) {
              break;
            }
            client_gone = true;
          }
          continue;
        }
        // The executor's stream ended before the process exited, such as
        // a runner's connection; the caller learns why, as when the
        // daemon ends a stream.
        Err(needy_api_client::Error::Ended { reason }) => {
          let reason = reason.unwrap_or_else(|| format!("the stream from executor {} ended", self.instance.name));
          if out_tx.send(Ok(ExecOutput::Ended(reason))).await.is_err() {
            tracing::debug!("exec relay: the client left before the stream ended");
          }
          break;
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
          daemon.remember(&started.process_id, &self.instance, self.origin, self.run.clone());
          self.record_start(&daemon, &process_id, started_ms).await;
          if let (Some(origin), Some(trace)) = (self.origin, &self.trace) {
            // From the request to the process running, placement and a
            // wait for the executor's slot included.
            trace.time("exec.start", self.requested.elapsed());
            run::emit(
              trace,
              &Event::ExecStart(ExecStart {
                process: process_id.clone(),
                executor: self.instance.name.clone(),
                argv: self.argv.clone(),
                origin,
                term_fallback: started.term_fallback.as_ref().map(|f| TermFallback {
                  requested: f.requested.clone(),
                  used: f.used.clone(),
                }),
                by: self.by.clone(),
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
          self.record_exit(&daemon, &process_id, &exit).await;
          if let (Some(origin), Some(trace)) = (self.origin, &self.trace) {
            run::emit(
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
        client_exec::Event::Detached => ExecOutput::Detached,
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
        if !self.detach.load(Ordering::SeqCst) {
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
