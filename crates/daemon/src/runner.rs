//! The runner role: a machine that joined a control plane. On its first
//! start it joins with a join token and keeps the credential it gets in
//! `runner.json` in its state directory (mode 0600); later starts reuse
//! that credential. It sends heartbeats while it runs, and dials the
//! control plane for work: it keeps one control stream open, connecting
//! again with backoff when it drops, and opens a task stream for each exec
//! the control plane announces (see the daemon's `link` module for the
//! control plane's side). It never listens.
//!
//! Every process a run starts here carries the run's ID. The runner reports
//! those still running whenever it connects, and ends a run's processes
//! when the control plane says the run ended. While it cannot reach the
//! control plane it keeps them for [`DISCONNECT_GRACE`], then ends them
//! itself.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, bail};
use backon::BackoffBuilder as _;
use futures_util::StreamExt as _;
use needy_api_client::proto::v1::process_service_server::ProcessService as _;
use needy_api_client::proto::v1::{
  ControlRequest, EndRunResponse, HeartbeatRequest, JoinRequest, KillProcessResponse, ListProcessesRequest, ProcessInfo, RunnerReport,
  TaskError, TaskRequest, TaskResult, control_request, control_response, runner_task, task_request, task_result,
};
use needy_api_client::{Client, Kind};
use needy_api_server::control::RunnerHost;
use needy_api_server::protocol::domain::{proto_runner_host, proto_runner_role};
use needy_core::types::RunnerRole;
use needy_core_executor::process::{Host, Input};
use needy_core_executor::service::ExecutorService;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// Where a runner keeps its credential, in its state directory.
const ENROLLMENT: &str = "runner.json";

// NOTICE: how long a runner waits to try again when the control plane does
// not answer a heartbeat. Shorter than the control plane's interval (15 s by
// default), so a runner shows as online soon after the control plane or a
// tunnel comes back; one small call every 5 s per waiting runner costs
// nothing.
const RETRY: Duration = Duration::from_secs(5);

// NOTICE: how a runner whose control stream dropped connects again: soon
// after a restart of the control plane or a tunnel (0.5 s, doubling, with
// jitter), and at most every 15 s while it stays away, the heartbeat
// interval's default, so a runner is back within one interval of the
// control plane.
const RECONNECT_MIN: Duration = Duration::from_millis(500);
const RECONNECT_MAX: Duration = Duration::from_secs(15);

// NOTICE: results of list and kill tasks queued for the control stream; they
// are small and answered as they come.
const RESULTS: usize = 64;

/// How long a runner that lost the control plane keeps the processes of
/// runs before it ends them itself.
// NOTICE: 2 minutes is eight reconnect attempts at the 15 s backoff cap
// (`RECONNECT_MAX`): enough for a control plane or a tunnel that restarts to
// come back and decide (claim the processes of live runs, end the others),
// while a control plane that is gone for good, such as one killed on another
// machine, leaves a run's processes and a workspace mount's SFTP server
// running for minutes, not hours. A restarted control plane fails the runs
// whose actors it lost anyway, so waiting longer keeps nothing useful alive.
pub const DISCONNECT_GRACE: Duration = Duration::from_secs(120);

// NOTICE: how long ending a run waits for its killed processes to exit, so
// the answer lists their exits. `SIGKILL` cannot be caught; a process still
// running after 5 s is stuck in the kernel and is reported running.
const END_WAIT: Duration = Duration::from_secs(5);

/// How a runner starts.
#[derive(Debug, Clone)]
pub struct Config {
  /// `runner.json`.
  pub state_dir: PathBuf,
  /// The control plane's endpoint; the one stored at the join when `None`.
  pub control: Option<String>,
  /// Needed on the first start only.
  pub join_token: Option<String>,
  pub name: String,
  // TODO(runner-roles): roles are declared by flags; detecting them (such as
  // `/dev/kvm` for the actor role) is a follow-up of slice 3.
  pub roles: BTreeSet<RunnerRole>,
  pub labels: BTreeMap<String, String>,
  /// How long it keeps the processes of runs while it cannot reach the
  /// control plane: [`DISCONNECT_GRACE`] outside tests.
  pub grace: Duration,
}

/// What a runner keeps between starts.
#[derive(Serialize, Deserialize)]
struct Enrollment {
  /// The endpoint it joined through.
  control: String,
  name: String,
  credential: String,
}

/// A runner that joined.
pub struct Runner {
  path: PathBuf,
  control: String,
  enrollment: Enrollment,
  roles: BTreeSet<RunnerRole>,
  labels: BTreeMap<String, String>,
  /// This machine, read once at start and reported in every heartbeat.
  host: RunnerHost,
  /// Connected on the first heartbeat, so a control plane that is not up
  /// yet only delays heartbeats.
  client: Option<Client>,
  /// The current credential, shared with the dialer, which presents it on
  /// every stream it opens.
  credential: Arc<Mutex<String>>,
  /// As the control plane last said; `None` until it said.
  interval: Option<Duration>,
  grace: Duration,
}

impl Runner {
  /// Loads the stored credential, or joins with the join token when there
  /// is none.
  pub async fn start(config: Config) -> Result<Self> {
    let path = config.state_dir.join(ENROLLMENT);
    let (enrollment, interval) = match load(&path)? {
      Some(stored) => {
        if stored.name != config.name {
          bail!(
            "this runner joined as {}, not {}; pass --name {} or delete {} to join again",
            stored.name,
            config.name,
            stored.name,
            path.display()
          );
        }
        (stored, None)
      }
      None => {
        let Some(join_token) = &config.join_token else {
          bail!("this runner has not joined yet: give it a join token with --join-token-env or --join-token-file");
        };
        let Some(control) = &config.control else {
          bail!("this runner has not joined yet: give it the control plane's endpoint with --join");
        };
        join(&config, control, join_token, &path).await?
      }
    };
    let control = config.control.clone().unwrap_or_else(|| enrollment.control.clone());
    Ok(Self {
      path,
      control,
      credential: Arc::new(Mutex::new(enrollment.credential.clone())),
      enrollment,
      roles: config.roles,
      labels: config.labels,
      host: host(),
      client: None,
      interval,
      grace: config.grace,
    })
  }

  /// The runner's name.
  pub fn name(&self) -> &str {
    &self.enrollment.name
  }

  /// Sends one heartbeat. A rotated credential in the answer is stored
  /// first, then used from the next call on.
  pub async fn beat(&mut self) -> Result<()> {
    let client = match &self.client {
      Some(client) => client.clone(),
      None => {
        let client = Client::connect(&self.control).await.with_context(|| format!("cannot reach the control plane at {}", self.control))?;
        let client = client.with_token(&self.enrollment.credential)?;
        self.client = Some(client.clone());
        client
      }
    };
    let request = HeartbeatRequest {
      roles: self.roles.iter().map(|&role| proto_runner_role(role) as i32).collect(),
      labels: self.labels.clone().into_iter().collect(),
      host: Some(proto_runner_host(&self.host)),
    };
    let beat = client.runners().heartbeat(request).await.map_err(needy_api_client::Error::from)?.into_inner();
    self.interval = interval(beat.heartbeat_interval_ms);
    if !beat.credential.is_empty() {
      self.enrollment.credential = beat.credential;
      store(&self.path, &self.enrollment)?;
      self.client = Some(client.with_token(&self.enrollment.credential)?);
      *self.credential.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = self.enrollment.credential.clone();
      tracing::info!("runner {}: stored a rotated credential", self.enrollment.name);
    }
    Ok(())
  }

  /// Sends heartbeats until `stop`, at the interval the control plane
  /// asks for. Fails when the control plane refuses the credential: the
  /// runner was removed, or its credential rotated away.
  pub async fn run(mut self, stop: CancellationToken) -> Result<()> {
    loop {
      let before = self.enrollment.credential.clone();
      let wait = match self.beat().await {
        // A rotated credential is used at once, which retires the old one
        // instead of leaving both valid for an interval.
        Ok(()) if self.enrollment.credential != before => Duration::ZERO,
        Ok(()) => self.interval.unwrap_or(RETRY),
        Err(e) if refused(&e) => return Err(refusal(&self.path)),
        Err(e) => {
          tracing::warn!("runner {}: heartbeat: {e:#}", self.enrollment.name);
          // The connection is made again for the next heartbeat.
          self.client = None;
          RETRY
        }
      };
      tokio::select! {
        () = stop.cancelled() => return Ok(()),
        () = tokio::time::sleep(wait) => {}
      }
    }
  }
}

/// This machine: its platform, and its CPU and KVM for the control plane's
/// recipe checks.
fn host() -> RunnerHost {
  let cpu = needy_core_actor::host::detect::detect().unwrap_or_else(|e| {
    tracing::warn!("cannot read this machine's CPU; the control plane skips recipe checks against it: {e}");
    Default::default()
  });
  RunnerHost {
    os: std::env::consts::OS.into(),
    arch: std::env::consts::ARCH.into(),
    cpu,
  }
}

/// The error a runner ends with once the control plane refuses its
/// credential.
fn refusal(path: &Path) -> anyhow::Error {
  anyhow::anyhow!(
    "the control plane refused this runner's credential: the runner was removed or its credential replaced; \
     delete {} and join again with a new join token",
    path.display()
  )
}

/// Whether the control plane refused the runner's credential, rather than
/// being unreachable.
fn refused(error: &anyhow::Error) -> bool {
  error.downcast_ref::<needy_api_client::Error>().is_some_and(|e| e.kind() == Kind::Unauthenticated)
}

/// Joins the control plane and stores the credential before anything
/// else, so a runner that stops right after does not lose it.
async fn join(config: &Config, control: &str, join_token: &str, path: &Path) -> Result<(Enrollment, Option<Duration>)> {
  let client = Client::connect(control).await.with_context(|| format!("cannot reach the control plane at {control}"))?;
  let request = JoinRequest {
    join_token: join_token.to_string(),
    name: config.name.clone(),
    roles: config.roles.iter().map(|&role| proto_runner_role(role) as i32).collect(),
    labels: config.labels.clone().into_iter().collect(),
  };
  let joined = client.runners().join(request).await.map_err(needy_api_client::Error::from).context("cannot join the control plane")?;
  let joined = joined.into_inner();
  let enrollment = Enrollment {
    control: control.to_string(),
    name: config.name.clone(),
    credential: joined.credential,
  };
  store(path, &enrollment)?;
  tracing::info!("joined the control plane at {control} as runner {}", config.name);
  Ok((enrollment, interval(joined.heartbeat_interval_ms)))
}

/// The heartbeat interval the control plane asked for; none for a zero or
/// negative one, which would send heartbeats without pause.
fn interval(ms: i64) -> Option<Duration> {
  u64::try_from(ms).ok().filter(|&ms| ms > 0).map(Duration::from_millis)
}

fn load(path: &Path) -> Result<Option<Enrollment>> {
  match std::fs::read_to_string(path) {
    Ok(text) => Ok(Some(serde_json::from_str(&text).with_context(|| format!("cannot read {}", path.display()))?)),
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
    Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
  }
}

/// Writes `enrollment` to a temporary file readable by this user only, then
/// renames it over `path`, so a crash leaves the old credential or the new
/// one, never half of one.
fn store(path: &Path, enrollment: &Enrollment) -> Result<()> {
  let dir = path.parent().context("the enrollment has no directory")?;
  #[cfg(unix)]
  needy_api_server::create_private_dir(dir).with_context(|| format!("cannot create {}", dir.display()))?;
  #[cfg(not(unix))]
  std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
  let partial = path.with_extension("json.partial");
  let mut options = std::fs::OpenOptions::new();
  options.write(true).create(true).truncate(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
  }
  // TODO(windows): the file keeps the directory's ACL on Windows; a runner
  // there relies on its state directory being private to the user.
  let write = || -> std::io::Result<()> {
    use std::io::Write as _;
    let mut file = options.open(&partial)?;
    file.write_all(&serde_json::to_vec_pretty(enrollment)?)?;
    file.sync_all()?;
    std::fs::rename(&partial, path)
  };
  write().with_context(|| format!("cannot write {}", path.display()))
}

impl Runner {
  /// Sends heartbeats and takes tasks from the control plane until `stop`,
  /// then kills the processes it started. Ends with an error when the
  /// control plane refuses the runner's credential.
  pub async fn serve(self, stop: CancellationToken) -> Result<()> {
    let ending = stop.child_token();
    let service = ExecutorService {
      host: Host::default(),
      name: self.name().to_string(),
    };
    let host = service.host.clone();
    let dialer = Dialer {
      control: self.control.clone(),
      name: self.name().to_string(),
      path: self.path.clone(),
      credential: self.credential.clone(),
      service,
      tasks: TaskTracker::new(),
      grace: self.grace,
      orphaned: Mutex::default(),
    };
    let tasks = dialer.tasks.clone();
    let dialing = {
      let ending = ending.clone();
      tokio::spawn(async move {
        let dialed = dialer.run(ending.clone()).await;
        ending.cancel();
        dialed
      })
    };
    let beaten = self.run(ending.clone()).await;
    ending.cancel();
    let dialed = dialing.await.context("the dialer failed")?;
    host.shutdown().await;
    tasks.close();
    tasks.wait().await;
    beaten.and(dialed)
  }
}

/// The runner's side of its control stream and task streams.
struct Dialer {
  control: String,
  name: String,
  /// `runner.json`, named when the credential is refused.
  path: PathBuf,
  credential: Arc<Mutex<String>>,
  service: ExecutorService,
  /// The tasks the control plane asked for; they end with `stop`.
  tasks: TaskTracker,
  /// How long the processes of runs outlive a lost control plane.
  grace: Duration,
  /// Set while the control plane is unreachable: cancelling it keeps the
  /// processes of runs that the grace period would end.
  orphaned: Mutex<Option<CancellationToken>>,
}

impl Dialer {
  /// Keeps the control stream open until `stop`, connecting again with
  /// backoff whenever it ends. Fails when the control plane refuses the
  /// credential.
  async fn run(&self, stop: CancellationToken) -> Result<()> {
    let backoff = || {
      backon::ExponentialBuilder::default()
        .with_min_delay(RECONNECT_MIN)
        .with_max_delay(RECONNECT_MAX)
        .with_jitter()
        .without_max_times()
        .build()
    };
    let mut delays = backoff();
    loop {
      let mut connected = false;
      let ended = tokio::select! {
        ended = self.session(&stop, &mut connected) => ended,
        () = stop.cancelled() => return Ok(()),
      };
      // The processes of runs wait for the control plane to come back.
      if connected {
        self.orphan(&stop);
      }
      match ended {
        Err(e) if refused(&e) => return Err(refusal(&self.path)),
        Err(e) => tracing::warn!("runner {}: control stream: {e:#}", self.name),
        Ok(()) => tracing::info!("runner {}: the control plane ended the control stream", self.name),
      }
      // A stream that was open starts the backoff over.
      if connected {
        delays = backoff();
      }
      let delay = delays.next().unwrap_or(RECONNECT_MAX);
      tokio::select! {
        () = tokio::time::sleep(delay) => {}
        () = stop.cancelled() => return Ok(()),
      }
    }
  }

  /// Starts the grace period of the processes of runs, unless one is
  /// running: they end when it passes before a control stream opens.
  fn orphan(&self, stop: &CancellationToken) {
    let mut orphaned = self.orphaned.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if orphaned.is_some() {
      return;
    }
    let claimed = CancellationToken::new();
    *orphaned = Some(claimed.clone());
    let (service, name, grace, stop) = (self.service.clone(), self.name.clone(), self.grace, stop.clone());
    self.tasks.spawn(async move {
      tokio::select! {
        () = tokio::time::sleep(grace) => {}
        () = claimed.cancelled() => return,
        () = stop.cancelled() => return,
      }
      let runs: BTreeSet<String> = run_processes(&service).await.into_iter().map(|p| p.run_id).collect();
      if runs.is_empty() {
        return;
      }
      tracing::warn!(
        "runner {name}: the control plane did not come back within {grace:?}; ending the processes of runs {}",
        runs.iter().cloned().collect::<Vec<_>>().join(", ")
      );
      for run in runs {
        end_run(&service, &run).await;
      }
    });
  }

  /// A control stream opened: the control plane decides about the
  /// processes of runs from the report, so the grace period stops.
  fn reconnected(&self) {
    let orphaned = self.orphaned.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
    if let Some(claimed) = orphaned {
      claimed.cancel();
    }
  }

  /// A client presenting the current credential.
  fn authenticated(&self, client: &Client) -> Result<Client> {
    let credential = self.credential.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
    Ok(client.clone().with_token(&credential)?)
  }

  /// One control stream, from connecting until it ends.
  async fn session(&self, stop: &CancellationToken, connected: &mut bool) -> Result<()> {
    let client =
      Client::connect_kept_alive(&self.control).await.with_context(|| format!("cannot reach the control plane at {}", self.control))?;
    let (results, rx) = mpsc::channel(RESULTS);
    // The first message says what the runner still runs for runs.
    let report = ControlRequest {
      payload: Some(control_request::Payload::Report(RunnerReport {
        processes: run_processes(&self.service).await,
      })),
    };
    results.send(report).await.context("the control stream closed")?;
    let mut tasks =
      self.authenticated(&client)?.runners().control(ReceiverStream::new(rx)).await.map_err(needy_api_client::Error::from)?.into_inner();
    *connected = true;
    self.reconnected();
    tracing::info!("runner {}: connected to the control plane at {}", self.name, self.control);
    while let Some(message) = tasks.message().await.map_err(needy_api_client::Error::from)? {
      let Some(control_response::Payload::Task(task)) = message.payload else {
        continue;
      };
      let Some(kind) = task.kind else {
        continue;
      };
      let id = task.id;
      let service = self.service.clone();
      let stop = stop.clone();
      match kind {
        runner_task::Kind::Exec(_) => {
          // A task stream presents the credential current when it opens.
          let client = self.authenticated(&client)?;
          let name = self.name.clone();
          self.tasks.spawn(async move {
            if let Some(Err(e)) = stop.run_until_cancelled(exec(client, service, id)).await {
              tracing::warn!("runner {name}: exec task {id}: {e:#}");
            }
          });
        }
        kind => {
          let results = results.clone();
          self.tasks.spawn(async move {
            let Some(result) = stop.run_until_cancelled(answer(&service, kind)).await else {
              return;
            };
            let message = ControlRequest {
              payload: Some(control_request::Payload::Result(TaskResult {
                task_id: id,
                result: Some(result),
              })),
            };
            // The stream ended; the control plane gave up on the task.
            if results.send(message).await.is_err() {
              tracing::debug!("task {id}: the control stream ended before its result");
            }
          });
        }
      }
    }
    Ok(())
  }
}

/// Carries out a task answered on the control stream.
async fn answer(service: &ExecutorService, kind: runner_task::Kind) -> task_result::Result {
  let answered = match kind {
    runner_task::Kind::ListProcesses(request) => {
      service.list_processes(tonic::Request::new(request)).await.map(|list| task_result::Result::ListProcesses(list.into_inner()))
    }
    runner_task::Kind::KillProcess(request) => {
      service.kill_process(tonic::Request::new(request)).await.map(|_| task_result::Result::KillProcess(KillProcessResponse {}))
    }
    runner_task::Kind::EndRun(request) => Ok(task_result::Result::EndRun(end_run(service, &request.run_id).await)),
    runner_task::Kind::Exec(_) => Err(tonic::Status::invalid_argument("an exec task has its own stream")),
  };
  answered.unwrap_or_else(|status| task_result::Result::Error(task_error(&status)))
}

/// The running processes that belong to a run.
async fn run_processes(service: &ExecutorService) -> Vec<ProcessInfo> {
  match service.list_processes(tonic::Request::new(ListProcessesRequest::default())).await {
    Ok(list) => list.into_inner().processes.into_iter().filter(|p| p.running && !p.run_id.is_empty()).collect(),
    // NOTICE: listing this machine's own table does not fail; an error
    // would mean nothing to report.
    Err(status) => {
      tracing::warn!("cannot list this runner's processes: {}", status.message());
      Vec::new()
    }
  }
}

/// Kills the running processes of run `run`, a workspace mount's SFTP
/// server among them, and waits a little for them to exit.
// TODO(runner-docker): a runner creates no containers yet (Docker executors
// run on the control host, which removes them); one that does removes the
// run's containers here too.
async fn end_run(service: &ExecutorService, run: &str) -> EndRunResponse {
  let procs: Vec<_> = service.host.list().into_iter().filter(|p| p.run_id == run && p.exit().is_none()).collect();
  let mut ended = Vec::new();
  for proc in &procs {
    // NOTICE: SIGKILL, as the executors' own shutdown uses: the run is over,
    // and a process that ignores SIGTERM would stay.
    proc.send(Input::Signal("SIGKILL".into()));
    ended.push(proc.id.clone());
  }
  if !procs.is_empty() {
    tracing::info!("run {run}: ended {} processes", procs.len());
  }
  let exits = futures_util::future::join_all(procs.iter().map(|p| p.wait()));
  if tokio::time::timeout(END_WAIT, exits).await.is_err() {
    tracing::warn!("run {run}: a killed process did not exit within {END_WAIT:?}");
  }
  let request = ListProcessesRequest {
    run_id: run.to_string(),
    ..Default::default()
  };
  let processes = match service.list_processes(tonic::Request::new(request)).await {
    Ok(list) => list.into_inner().processes,
    Err(status) => {
      tracing::warn!("run {run}: cannot list its processes: {}", status.message());
      Vec::new()
    }
  };
  EndRunResponse { processes, ended }
}

fn task_error(status: &tonic::Status) -> TaskError {
  TaskError {
    code: status.code() as i32,
    message: status.message().to_string(),
  }
}

/// Opens the task stream of exec task `id` and runs the exec on it: the
/// control plane's requests go to this machine's process service, its
/// responses back up. When the control plane leaves the stream, or the
/// connection ends, the exec's client is gone: a process it did not detach
/// ends as with any client that leaves.
async fn exec(client: Client, service: ExecutorService, id: u64) -> Result<()> {
  let (up, rx) = mpsc::channel::<TaskRequest>(RESULTS);
  let named = TaskRequest {
    payload: Some(task_request::Payload::TaskId(id)),
  };
  up.send(named).await.context("the task stream closed")?;
  let mut down = client.runners().task(ReceiverStream::new(rx)).await.map_err(needy_api_client::Error::from)?.into_inner();
  // Set once the control plane's side ends or fails, so the output below
  // stops with it.
  let left = CancellationToken::new();
  let (requests, inbound) = mpsc::channel(RESULTS);
  // NOTICE: the control plane's side is read until it ends, even after the
  // exec ended or was refused: dropping it would reset the stream, and the
  // reset can overtake the last responses (the refusal, the exit) still on
  // their way up.
  let pump = {
    let left = left.clone();
    async move {
      loop {
        match down.message().await {
          Ok(Some(message)) => {
            if let Some(request) = message.exec
              && requests.send(Ok(request)).await.is_err()
            {
              tracing::debug!("exec task {id}: a request after the exec ended");
            }
          }
          Ok(None) => break,
          Err(status) => {
            tracing::debug!("exec task {id}: task stream: {}", status.message());
            break;
          }
        }
      }
      left.cancel();
    }
  };
  let relay = async move {
    let error = |status: &tonic::Status| TaskRequest {
      payload: Some(task_request::Payload::Error(task_error(status))),
    };
    let mut responses = match service.open(Box::pin(ReceiverStream::new(inbound))).await {
      Ok(responses) => responses,
      Err(status) => {
        // The control plane gave up waiting; nothing to tell it.
        if up.send(error(&status)).await.is_err() {
          tracing::debug!("exec task {id}: the control plane left before the refusal");
        }
        return;
      }
    };
    loop {
      let response = tokio::select! {
        response = responses.next() => response,
        () = left.cancelled() => break,
      };
      let Some(response) = response else {
        break;
      };
      let (message, last) = match response {
        Ok(response) => (
          TaskRequest {
            payload: Some(task_request::Payload::Exec(response)),
          },
          false,
        ),
        Err(status) => (error(&status), true),
      };
      if up.send(message).await.is_err() || last {
        break;
      }
    }
  };
  tokio::join!(pump, relay);
  Ok(())
}

/// Joins or loads the credential, then sends heartbeats and takes tasks
/// from the control plane until a termination signal; the processes it
/// started are killed then. A second termination signal ends that where it
/// is and fails, naming the processes still running. Ends with an error when
/// the control plane refuses the runner's credential.
pub async fn serve(config: Config) -> Result<()> {
  let runner = Runner::start(config).await?;
  let termination = needy_api_server::termination::Termination::listen().context("cannot handle termination signals")?;
  tracing::info!("runner {} dialing the control plane at {}", runner.name(), runner.control);
  match termination.unless_forced(runner.serve(termination.stop())).await {
    Some(served) => served,
    None => {
      let message = "exited at a second termination signal before the runner's processes were killed";
      tracing::error!("{message}");
      bail!(message)
    }
  }
}

#[cfg(all(test, unix))]
#[path = "runner_test.rs"]
mod tests;
