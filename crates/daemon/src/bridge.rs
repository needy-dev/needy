//! Outbound actor host bridge: lifecycle, console log streaming, and reverse
//! terminal tunnel for remote actor runs (`CreateRunRequest.remote_actor`).
//!
//! A remote actor host connects outbound to `GuestService.Bridge` using the
//! run's per-run actor bearer token. At most one bridge may attach per run.
//! Console bytes streamed on the bridge are appended to `<run.dir>/console.log`
//! so `needy run logs` works identically to local runs. When the actor exits
//! (or the bridge stream drops unexpectedly), the daemon runs the same
//! completion path as `actor::watch` (`ActorVmExit`, `mirror_out`, executor
//! release, token revocation, and terminal `RunState`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures_util::StreamExt as _;
use needy_api_client::proto::v1::{
  BridgeAttach, BridgeAttached, BridgeExecTask, BridgeExit, BridgeRequest, BridgeResponse, BridgeStarted, BridgeStop, BridgeTaskResult,
  BridgeTerminalTask, ExecEnded, ExecRequest, ExecResponse, KillProcessRequest, ListProcessesRequest, ListProcessesResponse,
  TerminalTaskRequest, TerminalTaskResponse, bridge_request, bridge_response, bridge_task_result, bridge_terminal_task, exec_response,
  terminal_task_request,
};
use needy_api_client::{Executor, Responses};
use needy_api_server::control::{ControlError, FromRunner, ToRunner};
use needy_common::sync::lock;
use needy_common::time::now_ms;
use needy_core::event::{ActorReady, ActorStart, ActorVmExit, Event};
use needy_core::types::{ActorState, Capture, RunState};
use needy_core_actor::driver;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tonic::Status;

use crate::pool::Instance;
use crate::run::Run;
use crate::store::{self, InstanceKind, InstanceRecord, InstanceState};
use crate::{Daemon, actor};

const ANSWER: Duration = Duration::from_secs(10);
const QUEUE: usize = 64;
const ATTACH_TIMEOUT: Duration = Duration::from_secs(15);
const BRIDGE_STOP_WAIT: Duration = Duration::from_secs(4);

/// Per-run bridge attachment state, enforcing single-bridge exclusivity.
#[derive(Default)]
pub(crate) enum Slot {
  #[default]
  Idle,
  Attached(Arc<BridgeLink>),
  Closed,
}

/// Active outbound bridge control stream for one remote actor run.
pub(crate) struct BridgeLink {
  run: String,
  outbound: Mutex<Option<mpsc::Sender<BridgeResponse>>>,
  next: AtomicU64,
  pending: Mutex<HashMap<u64, Pending>>,
  closed: CancellationToken,
}

enum Pending {
  Reply(oneshot::Sender<bridge_task_result::Result>),
  Exec {
    requests: mpsc::Receiver<ExecRequest>,
    opened: oneshot::Sender<FromRunner<TerminalTaskRequest>>,
  },
}

struct PendingExec<'a> {
  link: &'a BridgeLink,
  id: u64,
}

impl Drop for PendingExec<'_> {
  fn drop(&mut self) {
    self.link.take(self.id);
  }
}

impl BridgeLink {
  fn pending(&self) -> MutexGuard<'_, HashMap<u64, Pending>> {
    lock(&self.pending)
  }

  fn take(&self, task: u64) -> Option<Pending> {
    self.pending().remove(&task)
  }

  fn close(&self) {
    self.closed.cancel();
    *lock(&self.outbound) = None;
    self.pending().clear();
  }

  fn disconnected(&self) -> Status {
    Status::unavailable(format!("actor bridge for run {} disconnected", self.run))
  }

  fn answer(&self, result: BridgeTaskResult) {
    let id = result.task_id;
    match (self.take(id), result.result) {
      (Some(Pending::Reply(reply)), Some(res)) => {
        if reply.send(res).is_err() {
          tracing::debug!("run {}: bridge terminal task {id} answered after caller left", self.run);
        }
      }
      (pending, _) => {
        tracing::debug!("run {}: unexpected bridge task result for task {id}", self.run);
        if let Some(exec @ Pending::Exec { .. }) = pending {
          self.pending().insert(id, exec);
        }
      }
    }
  }

  async fn announce(&self, id: u64, kind: bridge_terminal_task::Kind) -> Result<(), Status> {
    let Some(outbound) = lock(&self.outbound).clone() else {
      return Err(self.disconnected());
    };
    let message = BridgeResponse {
      payload: Some(bridge_response::Payload::TerminalTask(BridgeTerminalTask {
        id,
        kind: Some(kind),
      })),
    };
    tokio::select! {
      sent = outbound.send(message) => sent.map_err(|_| self.disconnected()),
      () = self.closed.cancelled() => Err(self.disconnected()),
    }
  }

  async fn ask(&self, kind: bridge_terminal_task::Kind) -> Result<bridge_task_result::Result, Status> {
    let id = self.next.fetch_add(1, Ordering::Relaxed);
    let (reply, answer) = oneshot::channel();
    self.pending().insert(id, Pending::Reply(reply));
    let answered = async {
      self.announce(id, kind).await?;
      tokio::select! {
        answer = answer => answer.map_err(|_| self.disconnected()),
        () = tokio::time::sleep(ANSWER) => Err(Status::unavailable(format!("actor bridge for run {} did not answer in time", self.run))),
      }
    }
    .await;
    self.take(id);
    match answered? {
      bridge_task_result::Result::Error(error) => Err(needy_api_client::diagnostic::task_status(error)),
      result => Ok(result),
    }
  }

  async fn exec(&self, requests: mpsc::Receiver<ExecRequest>) -> Result<Responses, Status> {
    let id = self.next.fetch_add(1, Ordering::Relaxed);
    let (opened, stream) = oneshot::channel();
    self.pending().insert(id, Pending::Exec { requests, opened });
    let pending = PendingExec { link: self, id };
    let opened = async {
      self.announce(id, bridge_terminal_task::Kind::Exec(BridgeExecTask {})).await?;
      tokio::select! {
        stream = stream => stream.map_err(|_| self.disconnected()),
        () = tokio::time::sleep(ANSWER) => Err(Status::unavailable(format!("actor bridge for run {} did not open terminal exec in time", self.run))),
      }
    }
    .await;
    drop(pending);
    let mut responses = opened?;
    match responses.next().await {
      Some(Ok(TerminalTaskRequest {
        payload: Some(terminal_task_request::Payload::Exec(first)),
      })) => {
        let rest = relayed(self.run.clone(), responses);
        Ok(Box::pin(tokio_stream::once(Ok(first)).chain(rest)))
      }
      Some(Ok(TerminalTaskRequest {
        payload: Some(terminal_task_request::Payload::Error(error)),
      })) => Err(needy_api_client::diagnostic::task_status(error)),
      Some(Ok(_)) => Err(Status::internal(format!("actor bridge for run {} sent no exec response first", self.run))),
      Some(Err(_)) | None => Err(self.disconnected()),
    }
  }
}

fn relayed(run: String, responses: FromRunner<TerminalTaskRequest>) -> Responses {
  let relayed = futures_util::stream::unfold(Some(responses), move |state| {
    let run = run.clone();
    async move {
      let mut responses = state?;
      loop {
        return match responses.next().await {
          Some(Ok(TerminalTaskRequest {
            payload: Some(terminal_task_request::Payload::Exec(response)),
          })) => Some((Ok(response), Some(responses))),
          Some(Ok(TerminalTaskRequest {
            payload: Some(terminal_task_request::Payload::Error(error)),
          })) => Some((Err(needy_api_client::diagnostic::task_status(error)), None)),
          Some(Ok(_)) => continue,
          Some(Err(failed)) => {
            tracing::debug!("run {run}: bridge terminal task stream: {}", failed.message());
            let ended = ExecResponse {
              diagnostic: None,
              payload: Some(exec_response::Payload::Ended(ExecEnded {
                reason: format!("actor bridge for run {run} disconnected"),
              })),
            };
            Some((Ok(ended), None))
          }
          None => None,
        };
      }
    }
  });
  Box::pin(relayed)
}

/// Reaches a remote actor's terminal `ProcessService` through its outbound
/// bridge control and task streams without dialing the actor host.
pub(crate) struct BridgeExecutor {
  link: Arc<BridgeLink>,
}

#[async_trait::async_trait]
impl Executor for BridgeExecutor {
  async fn exec(&self, requests: mpsc::Receiver<ExecRequest>) -> Result<Responses, Status> {
    self.link.exec(requests).await
  }

  async fn list_processes(&self, request: ListProcessesRequest) -> Result<ListProcessesResponse, Status> {
    match self.link.ask(bridge_terminal_task::Kind::ListProcesses(request)).await? {
      bridge_task_result::Result::ListProcesses(list) => Ok(list),
      _ => Err(Status::internal("actor bridge answered list_processes with unexpected payload")),
    }
  }

  async fn kill_process(&self, request: KillProcessRequest) -> Result<(), Status> {
    match self.link.ask(bridge_terminal_task::Kind::KillProcess(request)).await? {
      bridge_task_result::Result::KillProcess(_) => Ok(()),
      _ => Err(Status::internal("actor bridge answered kill_process with unexpected payload")),
    }
  }
}

fn parse_capture(text: &str, fallback: Capture) -> Capture {
  match text {
    "kernel" => Capture::Kernel,
    "shim" | "shims" => Capture::Shims,
    _ => fallback,
  }
}

struct StartMeta<'a> {
  pid: u32,
  vcpus: u32,
  mem_mib: u32,
  driver: &'a str,
  capture: &'a str,
}

async fn record_actor_start(daemon: &Daemon, run: &Run, started: &AtomicBool, meta: StartMeta<'_>) {
  if started.swap(true, Ordering::SeqCst) {
    return;
  }
  let facets = actor::facets(run);
  let driver_label = if meta.driver.is_empty() {
    run.profile.actor.driver.as_str().to_string()
  } else {
    meta.driver.to_string()
  };
  run.emit(&Event::ActorStart(ActorStart {
    pid: meta.pid,
    vcpus: meta.vcpus,
    mem_mib: meta.mem_mib,
    driver: Some(driver_label.clone()),
    capture: Some(parse_capture(meta.capture, facets.capture)),
  }));
  let record = InstanceRecord {
    id: store::actor_instance(&run.id),
    run: run.id.clone(),
    name: "actor".into(),
    kind: InstanceKind::Actor,
    runner: lock(&run.actor_runner).clone(),
    driver: driver_label,
    handle: format!("remote:{}", run.id),
    pid: (meta.pid > 0).then_some(i64::from(meta.pid)),
    state: InstanceState::Running,
    created_ms: now_ms(),
    ended_ms: None,
  };
  daemon.start_instance(&record).await;
}

impl Daemon {
  /// Attaches a remote actor host bridge to `run_id`. Rejects a second
  /// concurrent bridge with `ControlError::Failed`.
  pub(crate) async fn attach_bridge(
    &self,
    run_id: &str,
    mut inbound: FromRunner<BridgeRequest>,
  ) -> Result<ToRunner<BridgeResponse>, ControlError> {
    let run = self.runs.find(run_id)?;
    run.touch_activity();
    if !matches!(run.state.get(), RunState::Creating | RunState::Running) {
      return Err(ControlError::Failed(format!("run {} is not running ({})", run.id, run.state.get())));
    }
    let (tx, rx) = mpsc::channel(QUEUE);
    let link = Arc::new(BridgeLink {
      run: run.id.clone(),
      outbound: Mutex::new(Some(tx.clone())),
      next: AtomicU64::new(1),
      pending: Mutex::default(),
      closed: CancellationToken::new(),
    });
    // Single-bridge exclusivity: only one bridge may ever attach to a run.
    {
      let mut slot = lock(&run.bridge);
      match &*slot {
        Slot::Idle => *slot = Slot::Attached(link.clone()),
        Slot::Attached(_) => {
          return Err(ControlError::Failed(format!("run {} already has an actor bridge attached", run.id)));
        }
        Slot::Closed => {
          return Err(ControlError::Failed(format!("run {} actor bridge has already ended", run.id)));
        }
      }
    }

    // First message must be `BridgeAttach`.
    let first = tokio::time::timeout(ATTACH_TIMEOUT, inbound.next()).await;
    let attach = match first {
      Ok(Some(Ok(BridgeRequest {
        payload: Some(bridge_request::Payload::Attach(attach)),
      }))) => attach,
      Ok(Some(Ok(_))) => {
        *lock(&run.bridge) = Slot::Idle;
        link.close();
        return Err(ControlError::Invalid("the first message on GuestService.Bridge must be attach".into()));
      }
      Ok(Some(Err(status))) => {
        *lock(&run.bridge) = Slot::Idle;
        link.close();
        return Err(ControlError::Unavailable(format!("actor bridge stream failed before attach: {}", status.message())));
      }
      Ok(None) => {
        *lock(&run.bridge) = Slot::Idle;
        link.close();
        return Err(ControlError::Invalid("actor bridge stream closed before attach".into()));
      }
      Err(_) => {
        *lock(&run.bridge) = Slot::Idle;
        link.close();
        return Err(ControlError::Unavailable("timed out waiting for actor bridge attach".into()));
      }
    };

    let recipe_json = match &run.recipe {
      Some(recipe) => serde_json::to_string(recipe).map_err(|e| ControlError::Internal(format!("cannot serialize recipe: {e}")))?,
      None => String::new(),
    };
    let attached = BridgeResponse {
      payload: Some(bridge_response::Payload::Attached(BridgeAttached {
        run_id: run.id.clone(),
        profile_yaml: run.profile_yaml.clone(),
        recipe_json,
        tty: run.tty.is_some(),
      })),
    };
    if tx.send(attached).await.is_err() {
      *lock(&run.bridge) = Slot::Closed;
      link.close();
      return Err(ControlError::Unavailable("actor bridge outbound channel closed".into()));
    }

    let started = Arc::new(AtomicBool::new(false));
    let BridgeAttach {
      pid,
      vcpus,
      mem_mib,
      driver: attach_driver,
      capture: attach_capture,
    } = attach;
    if pid > 0 || !attach_driver.is_empty() {
      record_actor_start(
        self,
        &run,
        &started,
        StartMeta {
          pid,
          vcpus,
          mem_mib,
          driver: &attach_driver,
          capture: &attach_capture,
        },
      )
      .await;
    }

    let daemon = self.clone();
    let task_run = run.clone();
    let task_link = link.clone();
    let driver_name = if attach_driver.is_empty() {
      run.profile.actor.driver.as_str().to_string()
    } else {
      attach_driver
    };
    run.tasks.spawn(async move {
      drive_bridge(daemon, task_run, task_link, inbound, tx, started, driver_name).await;
    });

    // NOTICE: do not wrap `rx` in `take_until(closed.cancelled_owned())`;
    // `link.close()` drops all senders so any final message (such as
    // `BridgeStop`) is delivered to the peer before the stream ends.
    Ok(Box::pin(ReceiverStream::new(rx)))
  }

  /// Hands a reverse terminal exec stream opened by the bridge to the
  /// waiting `BridgeExecutor::exec` caller.
  pub(crate) fn bridge_terminal_task(
    &self,
    run_id: &str,
    task: u64,
    responses: FromRunner<TerminalTaskRequest>,
  ) -> Result<ToRunner<TerminalTaskResponse>, ControlError> {
    let run = self.runs.find(run_id)?;
    run.touch_activity();
    let link = match &*lock(&run.bridge) {
      Slot::Attached(link) => link.clone(),
      Slot::Idle | Slot::Closed => {
        return Err(ControlError::NotFound(format!("run {} has no attached actor bridge", run.id)));
      }
    };
    let Some(Pending::Exec { requests, opened }) = link.take(task) else {
      return Err(ControlError::NotFound(format!("run {} has no bridge terminal task {task}", run.id)));
    };
    opened.send(responses).map_err(|_| ControlError::NotFound(format!("bridge terminal task {task} was given up")))?;
    let requests = ReceiverStream::new(requests).map(|request| TerminalTaskResponse {
      exec: Some(request),
    });
    Ok(Box::pin(requests))
  }
}

async fn drive_bridge(
  daemon: Daemon,
  run: Arc<Run>,
  link: Arc<BridgeLink>,
  mut inbound: FromRunner<BridgeRequest>,
  outbound: mpsc::Sender<BridgeResponse>,
  started: Arc<AtomicBool>,
  mut driver_name: String,
) {
  let console_path = driver::log(&run.dir);
  let mut console_file: Option<tokio::fs::File> = None;
  let mut reported_exit: Option<BridgeExit> = None;
  let mut stop_sent = false;
  let mut stop_deadline: Option<tokio::time::Instant> = None;

  loop {
    let message = tokio::select! {
      message = inbound.next() => message,
      () = run.cancel.cancelled(), if !stop_sent => {
        stop_sent = true;
        if daemon.suspending.load(Ordering::SeqCst) {
          break;
        }
        // Tell the remote bridge to stop its microVM and wait briefly for
        // final console bytes / `BridgeExit` before closing the stream.
        let _ = tokio::time::timeout(
          Duration::from_secs(1),
          outbound.send(BridgeResponse {
            payload: Some(bridge_response::Payload::Stop(BridgeStop {
              grace_ms: actor::VM_STOP_GRACE.as_millis() as i64,
            })),
          }),
        )
        .await;
        stop_deadline = Some(tokio::time::Instant::now() + BRIDGE_STOP_WAIT);
        continue;
      }
      () = async {
        tokio::time::sleep_until(stop_deadline.expect("guarded by is_some")).await;
      }, if stop_deadline.is_some() => break,
      () = link.closed.cancelled() => break,
    };
    match message {
      Some(Ok(BridgeRequest {
        payload:
          Some(bridge_request::Payload::Started(BridgeStarted {
            pid,
            vcpus,
            mem_mib,
            driver,
            capture,
          })),
      })) => {
        run.touch_activity();
        if !driver.is_empty() {
          driver_name = driver.clone();
        }
        record_actor_start(
          &daemon,
          &run,
          &started,
          StartMeta {
            pid,
            vcpus,
            mem_mib,
            driver: &driver,
            capture: &capture,
          },
        )
        .await;
      }
      Some(Ok(BridgeRequest {
        payload: Some(bridge_request::Payload::Console(bytes)),
      })) => {
        run.touch_activity();
        if !bytes.is_empty() {
          if console_file.is_none() {
            match tokio::fs::OpenOptions::new().create(true).append(true).open(&console_path).await {
              Ok(file) => console_file = Some(file),
              Err(e) => tracing::warn!("run {}: cannot open {}: {e}", run.id, console_path.display()),
            }
          }
          if let Some(file) = &mut console_file {
            if let Err(e) = file.write_all(&bytes).await {
              tracing::warn!("run {}: cannot write {}: {e}", run.id, console_path.display());
            } else {
              let _ = file.flush().await;
            }
          }
        }
      }
      Some(Ok(BridgeRequest {
        payload: Some(bridge_request::Payload::TaskResult(result)),
      })) => {
        run.touch_activity();
        link.answer(result);
      }
      Some(Ok(BridgeRequest {
        payload: Some(bridge_request::Payload::TerminalReady(_)),
      })) => {
        run.touch_activity();
        let executor = Instance::actor(&run.id, Arc::new(BridgeExecutor { link: link.clone() }));
        // NOTICE: a second report from the same bridge changes nothing: the
        // run keeps the terminal it was first given.
        let _ = run.bridged_terminal.set(executor);
        if actor::advance(&run, ActorState::Ready) {
          run.emit(&Event::ActorReady(ActorReady {}));
        }
      }
      Some(Ok(BridgeRequest {
        payload: Some(bridge_request::Payload::Exit(exit)),
      })) => {
        run.touch_activity();
        reported_exit = Some(exit);
        break;
      }
      Some(Ok(BridgeRequest {
        payload: Some(bridge_request::Payload::Attach(_)) | None,
      })) => {}
      Some(Err(status)) => {
        tracing::warn!("run {}: actor bridge stream error: {}", run.id, status.message());
        break;
      }
      None => break,
    }
  }

  drop(outbound);
  *lock(&run.bridge) = Slot::Closed;
  link.close();
  actor::advance(&run, ActorState::Exited);

  if stop_sent {
    if daemon.suspending.load(Ordering::SeqCst) {
      return;
    }
    actor::release_actor(&daemon, &run).await;
    return;
  }

  let (exit, lost) = match reported_exit {
    Some(exit) => (
      ActorVmExit {
        code: exit.code,
        signal: (!exit.signal.is_empty()).then_some(exit.signal),
        error: (!exit.error.is_empty()).then_some(exit.error),
      },
      false,
    ),
    None => (
      ActorVmExit {
        code: None,
        signal: None,
        error: Some("actor bridge disconnected".into()),
      },
      true,
    ),
  };

  if lost {
    daemon.advance_instance(&run.id, &store::actor_instance(&run.id), InstanceState::Lost, now_ms()).await;
  } else {
    actor::release_actor(&daemon, &run).await;
  }

  run.emit(&Event::ActorVmExit(exit.clone()));
  if run.state.get() == RunState::Stopping {
    return;
  }
  if !lost && run.actor_exit().is_some() {
    if let Err(refused) = run.state.advance(RunState::Finished) {
      tracing::debug!("run {}: {refused}", run.id);
    }
  } else {
    let reason = actor::vm_failure(&driver_name, &exit, &console_path);
    tracing::warn!("run {} failed: {reason}", run.id);
    run.fail(reason);
  }
  daemon.finish_run(&run).await;
  run.trace.flush().await;
}

#[cfg(test)]
#[path = "bridge_test.rs"]
mod tests;
