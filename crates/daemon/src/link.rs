//! Runners that dialed the control plane: their control streams, the tasks
//! announced on them, and the task streams they open for execs. The daemon
//! reaches a connected runner's machine through [`RunnerExecutor`], an
//! [`Executor`] like any other, so the relay, `ps`, `kill` and file copies
//! work on it unchanged.
//!
//! A runner keeps one control stream (`RunnerService.Control`). For each
//! task the control plane announces a `RunnerTask` with an ID on it. A task
//! with one request and one reply (list or kill processes) is answered on
//! the control stream as a `TaskResult`. For an exec the runner opens a task
//! stream (`RunnerService.Task`) whose first request names the task; the
//! exec's own `ExecRequest`s go down it and its `ExecResponse`s come up.
//!
//! A runner's first message on a control stream is its report: the
//! processes of runs it still runs. The link hands it to the daemon, which
//! claims those of live runs and ends the others (`crate::reconcile`); an
//! `EndRun` task ends a run's processes on the runner.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures_util::StreamExt as _;
use needy_api_client::proto::v1::{
  ControlRequest, ControlResponse, EndRun, EndRunResponse, ExecEnded, ExecRequest, ExecResponse, ExecTask, KillProcessRequest,
  ListProcessesRequest, ListProcessesResponse, RunnerReport, RunnerTask, TaskError, TaskRequest, TaskResponse, control_request,
  control_response, exec_response, runner_task, task_request, task_result,
};
use needy_api_client::{Executor, Responses};
use needy_api_server::control::{ControlError, FromRunner, ToRunner};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tonic::{Code, Status};

// NOTICE: how long the control plane waits for a runner to open an exec's
// task stream, or to answer a list or a kill. A connected runner does either
// within one round trip; 10 s covers a slow tunnel without leaving a caller
// waiting on a runner whose connection died without a reset.
const ANSWER: Duration = Duration::from_secs(10);

// NOTICE: tasks announced but not yet sent to the runner. The runner reads
// its control stream as fast as it can spawn tasks, so a short queue only
// absorbs bursts such as `ps` across many processes.
const QUEUE: usize = 64;

/// What the daemon does with a runner's report: called with the runner's
/// name.
pub(crate) type OnReport = Box<dyn Fn(&str, RunnerReport) + Send + Sync>;

/// The connected runners, by name.
#[derive(Default)]
pub(crate) struct Links {
  links: Mutex<HashMap<String, Arc<Link>>>,
  /// Notified whenever a runner's control stream opens.
  opened: tokio::sync::Notify,
}

/// One runner's control stream.
pub(crate) struct Link {
  runner: String,
  tasks: mpsc::Sender<ControlResponse>,
  next: AtomicU64,
  pending: Mutex<HashMap<u64, Pending>>,
  /// Cancelled when the stream ends or is replaced; the tasks waiting on it
  /// fail at once.
  closed: CancellationToken,
}

/// A task announced and not yet answered.
enum Pending {
  /// Answered on the control stream.
  Reply(oneshot::Sender<task_result::Result>),
  /// Waiting for the runner to open the task's stream, which gets
  /// `requests`; the runner's side of the stream goes to `opened`.
  Exec {
    requests: mpsc::Receiver<ExecRequest>,
    opened: oneshot::Sender<FromRunner<TaskRequest>>,
  },
}

impl Links {
  fn lock(&self) -> MutexGuard<'_, HashMap<String, Arc<Link>>> {
    // NOTICE: every change under the lock is one map update, so a panic
    // elsewhere leaves the map whole.
    self.links.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }

  /// The runner's link while its control stream is open.
  pub(crate) fn get(&self, runner: &str) -> Option<Arc<Link>> {
    self.lock().get(runner).cloned()
  }

  /// The runner's link once its control stream is open, waiting up to
  /// `wait` for it; `None` when it did not open by then.
  pub(crate) async fn connected(&self, runner: &str, wait: Duration) -> Option<Arc<Link>> {
    let waited = tokio::time::timeout(wait, async {
      loop {
        // Registered before the check, so an open between the two wakes it.
        let opened = self.opened.notified();
        if let Some(link) = self.get(runner) {
          return link;
        }
        opened.await;
      }
    });
    waited.await.ok()
  }

  /// The names of the connected runners, sorted.
  pub(crate) fn names(&self) -> Vec<String> {
    let mut names: Vec<String> = self.lock().keys().cloned().collect();
    names.sort();
    names
  }

  /// Opens the control stream of `runner`, replacing an earlier one, and
  /// returns the tasks to send it. `results` are read by a task in `tasks`
  /// until the runner's side ends or `cancel`; the runner's report goes to
  /// `on_report`.
  pub(crate) fn open(
    self: &Arc<Self>,
    runner: &str,
    mut results: FromRunner<ControlRequest>,
    tasks: &TaskTracker,
    cancel: &CancellationToken,
    on_report: OnReport,
  ) -> ToRunner<ControlResponse> {
    let (tx, rx) = mpsc::channel(QUEUE);
    let link = Arc::new(Link {
      runner: runner.to_string(),
      tasks: tx,
      next: AtomicU64::new(1),
      pending: Mutex::default(),
      closed: CancellationToken::new(),
    });
    if let Some(earlier) = self.lock().insert(runner.to_string(), link.clone()) {
      tracing::info!("runner {runner}: a new control stream replaces the earlier one");
      earlier.close();
    }
    tracing::info!("runner {runner} connected");
    self.opened.notify_waiters();
    let links = self.clone();
    let reader = link.clone();
    let stop = cancel.clone();
    tasks.spawn(async move {
      loop {
        let message = tokio::select! {
          message = results.next() => message,
          () = reader.closed.cancelled() => break,
          () = stop.cancelled() => break,
        };
        match message {
          Some(Ok(ControlRequest {
            payload: Some(control_request::Payload::Report(report)),
          })) => on_report(&reader.runner, report),
          Some(Ok(message)) => reader.answer(message),
          Some(Err(status)) => {
            tracing::info!("runner {}: control stream: {}", reader.runner, status.message());
            break;
          }
          None => break,
        }
      }
      links.forget(&reader);
    });
    let closed = link.closed.clone();
    Box::pin(ReceiverStream::new(rx).take_until(closed.cancelled_owned()))
  }

  /// Ends the runner's control stream, if it has one; the runner connects
  /// again, or learns at once that it was removed.
  pub(crate) fn close(&self, runner: &str) {
    let link = self.lock().remove(runner);
    if let Some(link) = link {
      link.close();
    }
  }

  /// Ends every control stream, as the daemon stops serving.
  pub(crate) fn close_all(&self) {
    let links: Vec<Arc<Link>> = self.lock().drain().map(|(_, link)| link).collect();
    for link in links {
      link.close();
    }
  }

  /// Drops `link` once its stream ended, unless a newer stream replaced it.
  fn forget(&self, link: &Arc<Link>) {
    {
      let mut links = self.lock();
      if links.get(&link.runner).is_some_and(|current| Arc::ptr_eq(current, link)) {
        links.remove(&link.runner);
        tracing::info!("runner {} disconnected", link.runner);
      }
    }
    link.close();
  }

  /// Tells `runner` that run `run` ended: it kills the run's processes.
  /// Returns the run's processes as the runner left them.
  pub(crate) async fn end_run(&self, runner: &str, run: &str) -> Result<EndRunResponse, Status> {
    let link = self.get(runner).ok_or_else(|| Status::unavailable(not_connected(runner)))?;
    let task = runner_task::Kind::EndRun(EndRun {
      run_id: run.to_string(),
    });
    match link.ask(task).await? {
      task_result::Result::EndRun(ended) => Ok(ended),
      _ => Err(Status::internal(format!("runner {runner} answered the end of a run with something else"))),
    }
  }

  /// Hands the task stream `runner` opened for `task` to the exec waiting
  /// for it, and returns the exec's requests to send down that stream.
  pub(crate) fn task(&self, runner: &str, task: u64, responses: FromRunner<TaskRequest>) -> Result<ToRunner<TaskResponse>, ControlError> {
    let link = self.get(runner).ok_or_else(|| ControlError::NotFound(format!("runner {runner} has no control stream")))?;
    let Some(Pending::Exec { requests, opened }) = link.take(task) else {
      return Err(ControlError::NotFound(format!("runner {runner} has no exec task {task}")));
    };
    // The exec gave up waiting; the runner's stream ends with this error.
    opened.send(responses).map_err(|_| ControlError::NotFound(format!("exec task {task} was given up")))?;
    let requests = ReceiverStream::new(requests).map(|request| TaskResponse {
      exec: Some(request),
    });
    Ok(Box::pin(requests))
  }
}

impl Link {
  fn pending(&self) -> MutexGuard<'_, HashMap<u64, Pending>> {
    // NOTICE: as in `Links::lock`.
    self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }

  fn take(&self, task: u64) -> Option<Pending> {
    self.pending().remove(&task)
  }

  /// Ends the stream; every task waiting on it fails.
  fn close(&self) {
    self.closed.cancel();
    self.pending().clear();
  }

  /// A result the runner sent on its control stream.
  fn answer(&self, message: ControlRequest) {
    let Some(control_request::Payload::Result(result)) = message.payload else {
      return;
    };
    let id = result.task_id;
    match (self.take(id), result.result) {
      (Some(Pending::Reply(reply)), Some(result)) => {
        if reply.send(result).is_err() {
          tracing::debug!("runner {}: task {id} answered after its caller gave up", self.runner);
        }
      }
      (pending, _) => {
        tracing::debug!("runner {}: unexpected result for task {id}", self.runner);
        // An exec task answered on the control stream is put back for its
        // task stream.
        if let Some(exec @ Pending::Exec { .. }) = pending {
          self.pending().insert(id, exec);
        }
      }
    }
  }

  /// The runner's connection ended, as an error.
  fn disconnected(&self) -> Status {
    Status::unavailable(format!("runner {} disconnected", self.runner))
  }

  /// Announces task `id` on the control stream.
  async fn announce(&self, id: u64, kind: runner_task::Kind) -> Result<(), Status> {
    let task = ControlResponse {
      payload: Some(control_response::Payload::Task(RunnerTask {
        id,
        kind: Some(kind),
      })),
    };
    tokio::select! {
      sent = self.tasks.send(task) => sent.map_err(|_| self.disconnected()),
      () = self.closed.cancelled() => Err(self.disconnected()),
    }
  }

  /// A task answered on the control stream.
  async fn ask(&self, kind: runner_task::Kind) -> Result<task_result::Result, Status> {
    let id = self.next.fetch_add(1, Ordering::Relaxed);
    let (reply, answer) = oneshot::channel();
    self.pending().insert(id, Pending::Reply(reply));
    let answered = async {
      self.announce(id, kind).await?;
      tokio::select! {
        answer = answer => answer.map_err(|_| self.disconnected()),
        () = tokio::time::sleep(ANSWER) => Err(Status::unavailable(format!("runner {} did not answer in time", self.runner))),
      }
    };
    let answered = answered.await;
    self.take(id);
    match answered? {
      task_result::Result::Error(error) => Err(status(error)),
      result => Ok(result),
    }
  }

  /// An exec task: announced on the control stream, carried on the task
  /// stream the runner opens for it. Returns once the runner's executor
  /// started or attached, or with the status it refused with.
  async fn exec(&self, requests: mpsc::Receiver<ExecRequest>) -> Result<Responses, Status> {
    let id = self.next.fetch_add(1, Ordering::Relaxed);
    let (opened, stream) = oneshot::channel();
    self.pending().insert(id, Pending::Exec { requests, opened });
    let opened = async {
      self.announce(id, runner_task::Kind::Exec(ExecTask {})).await?;
      tokio::select! {
        stream = stream => stream.map_err(|_| self.disconnected()),
        () = tokio::time::sleep(ANSWER) => Err(Status::unavailable(format!("runner {} did not open the exec in time", self.runner))),
      }
    };
    let opened = opened.await;
    // Still pending when the runner never opened the stream.
    self.take(id);
    let mut responses = opened?;
    match responses.next().await {
      Some(Ok(TaskRequest {
        payload: Some(task_request::Payload::Exec(first)),
      })) => {
        let rest = relayed(self.runner.clone(), responses);
        Ok(Box::pin(tokio_stream::once(Ok(first)).chain(rest)))
      }
      Some(Ok(TaskRequest {
        payload: Some(task_request::Payload::Error(error)),
      })) => Err(status(error)),
      Some(Ok(_)) => Err(Status::internal(format!("runner {} sent no exec response first", self.runner))),
      Some(Err(_)) | None => Err(self.disconnected()),
    }
  }
}

/// A runner's exec responses as the executor's. A stream that fails, as
/// when the runner's connection ends, says so with `ended`, which a client
/// reports as the connection ending before the command exited.
fn relayed(runner: String, responses: FromRunner<TaskRequest>) -> Responses {
  let relayed = futures_util::stream::unfold(Some(responses), move |state| {
    let runner = runner.clone();
    async move {
      let mut responses = state?;
      loop {
        return match responses.next().await {
          Some(Ok(TaskRequest {
            payload: Some(task_request::Payload::Exec(response)),
          })) => Some((Ok(response), Some(responses))),
          Some(Ok(TaskRequest {
            payload: Some(task_request::Payload::Error(error)),
          })) => Some((Err(status(error)), None)),
          // A second task ID, or a payload this build does not know.
          Some(Ok(_)) => continue,
          Some(Err(failed)) => {
            tracing::debug!("runner {runner}: task stream: {}", failed.message());
            let ended = ExecResponse {
              payload: Some(exec_response::Payload::Ended(ExecEnded {
                reason: format!("runner {runner} disconnected"),
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

/// A runner's error as the status its process service returned.
fn status(error: TaskError) -> Status {
  Status::new(Code::from_i32(error.code), error.message)
}

/// A connected runner's machine, reached through its control stream. It
/// looks the runner's current link up on every call, so it keeps working
/// after the runner connects again.
pub(crate) struct RunnerExecutor {
  pub runner: String,
  pub links: Arc<Links>,
}

impl RunnerExecutor {
  fn link(&self) -> Result<Arc<Link>, Status> {
    self.links.get(&self.runner).ok_or_else(|| Status::unavailable(not_connected(&self.runner)))
  }
}

/// Why a task cannot reach `runner`.
pub(crate) fn not_connected(runner: &str) -> String {
  format!("runner {runner} is not connected")
}

#[async_trait::async_trait]
impl Executor for RunnerExecutor {
  async fn exec(&self, requests: mpsc::Receiver<ExecRequest>) -> Result<Responses, Status> {
    self.link()?.exec(requests).await
  }

  async fn list_processes(&self, request: ListProcessesRequest) -> Result<ListProcessesResponse, Status> {
    match self.link()?.ask(runner_task::Kind::ListProcesses(request)).await? {
      task_result::Result::ListProcesses(list) => Ok(list),
      _ => Err(Status::internal(format!("runner {} answered a list with something else", self.runner))),
    }
  }

  async fn kill_process(&self, request: KillProcessRequest) -> Result<(), Status> {
    match self.link()?.ask(runner_task::Kind::KillProcess(request)).await? {
      task_result::Result::KillProcess(_) => Ok(()),
      _ => Err(Status::internal(format!("runner {} answered a kill with something else", self.runner))),
    }
  }
}

#[cfg(test)]
#[path = "link_test.rs"]
mod tests;
