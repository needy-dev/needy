//! Runs: create, get, list and stop, through `api-server`'s `Runs`
//! trait, and the registry of live and ended runs.
//!
//! A run is registered while it is created, so it is listed as
//! `creating`. A create that fails unwinds what it did and is kept as a
//! failed run; a stopped run is kept as stopped, so `run ls`,
//! `run logs` and `report` still find it.

pub mod state;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context, Result, bail};
use needy_api_client::proto::v1::{KillProcessRequest, ListProcessesRequest};
use needy_api_server::control::{ControlError, NewRun, Route, RunInfo, Runs};
use needy_common::process::Exit;
use needy_common::time::now_ms;
use needy_config::Source;
use needy_config::profile::{Profile, TraceSink};
use needy_config::validate::{self, Severity};
use needy_core::chaos::Injector;
use needy_core::event::{self, Event, ExecutorRef, RunCreate, RunFail, RunStop};
use needy_core::intercept::ExecContext;
use needy_core::recipe::CompiledRecipe;
use needy_core::types::RunState;
use needy_core_actor::host::vm;
use needy_core_executor::provider::InstanceSpec;
use needy_trace::Dispatch;
use needy_trace::sink::Sink;
use needy_trace::sink::jsonl::Jsonl;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::pool::Instance;
use crate::{Daemon, actor, failed, files, routing};
use state::{Refused, State};

/// Events a run's trace queue holds before it drops them.
// NOTICE: a burst larger than this while the sinks are slow is recorded as a
// `trace.gap`; 4096 envelopes are a few MiB at most.
const TRACE_QUEUE: usize = 4096;

/// A live run: being created, running, finished or stopping.
pub struct Run {
  pub id: String,
  pub name: String,
  pub group: String,
  pub profile_name: String,
  pub profile: Profile,
  pub created_ms: i64,
  pub dir: PathBuf,
  pub state: State,
  /// The actor's driving program.
  pub actor_command: Vec<String>,
  pub recipe: Option<CompiledRecipe>,
  pub injector: Arc<Injector>,
  pub trace: Dispatch,
  /// The run's executors by name, added as they are acquired.
  executors: Mutex<BTreeMap<String, Arc<Instance>>>,
  /// The executors it gave back, for `run ls`.
  released: Mutex<Vec<ExecutorRef>>,
  /// How the actor's driving program ended, as its init reported it.
  actor_exit: Mutex<Option<Exit>>,
  /// Why the run failed after it was created, such as an actor whose VM
  /// exited before its driving program did.
  failure: Mutex<Option<String>>,
  /// The run's tasks: the relays of its commands, and its actor's
  /// guest listener, gateway and VM watch, which run under a child token.
  pub tasks: TaskTracker,
  pub cancel: CancellationToken,
}

impl Run {
  /// Records `event` in the run's trace.
  pub fn emit(&self, event: &Event) {
    emit(&self.trace, event);
  }

  pub fn executor(&self, name: &str) -> Option<Arc<Instance>> {
    self.lock_executors().get(name).cloned()
  }

  pub fn executors(&self) -> BTreeMap<String, Arc<Instance>> {
    self.lock_executors().clone()
  }

  /// Takes the run's executors out of it, to release them, and keeps
  /// them listed as released.
  fn take_executors(&self) -> BTreeMap<String, Arc<Instance>> {
    let taken = std::mem::take(&mut *self.lock_executors());
    lock(&self.released).extend(executor_refs(&taken));
    taken
  }

  /// Records how the actor's driving program ended.
  pub fn set_actor_exit(&self, exit: Exit) {
    *lock(&self.actor_exit) = Some(exit);
  }

  /// How the actor's driving program ended, once its init reported it.
  pub fn actor_exit(&self) -> Option<Exit> {
    lock(&self.actor_exit).clone()
  }

  /// Fails the run for `reason`, unless it already ended, and records
  /// the reason as `run.fail`.
  pub fn fail(&self, reason: String) {
    // The reason is in place before the state says `failed`.
    *lock(&self.failure) = Some(reason.clone());
    match self.state.advance(RunState::Failed) {
      Ok(_) => self.emit(&Event::RunFail(RunFail { reason })),
      Err(refused) => {
        tracing::debug!("run {}: {refused}", self.id);
        *lock(&self.failure) = None;
      }
    }
  }

  pub fn info(&self) -> RunInfo {
    RunInfo {
      id: self.id.clone(),
      name: self.name.clone(),
      group: self.group.clone(),
      state: self.state.get(),
      executors: executor_refs(&self.executors()),
      released: lock(&self.released).clone(),
      created_ms: self.created_ms,
      profile: self.profile_name.clone(),
      // NOTICE: an actor whose VM ended before init reported the driving
      // program's exit has none; `actor.vm.exit` in the trace has the VM's.
      actor_exit: lock(&self.actor_exit).clone(),
      failure: lock(&self.failure).clone(),
    }
  }

  fn lock_executors(&self) -> MutexGuard<'_, BTreeMap<String, Arc<Instance>>> {
    // NOTICE: each change is one insert, so a panic elsewhere leaves the map
    // whole.
    self.executors.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }
}

pub fn emit(trace: &Dispatch, event: &Event) {
  trace.emit(event.kind().as_str(), event.raw());
}

fn executor_refs(executors: &BTreeMap<String, Arc<Instance>>) -> Vec<ExecutorRef> {
  executors
    .iter()
    .map(|(name, instance)| ExecutorRef {
      name: name.clone(),
      runtime: instance.handle.runtime.into(),
      instance: instance.handle.id.clone(),
    })
    .collect()
}

// ------------------------------------------------------------- registry

/// Live runs in the order they were created, and ended ones.
#[derive(Default)]
pub struct Registry {
  live: Mutex<Vec<Arc<Run>>>,
  /// Runs that were stopped or failed to create, kept so `run ls`
  /// and `run logs` still find them.
  // TODO(persistence): this list lives in memory and grows until the daemon
  // exits; slice 9 keeps runs in `state.db`.
  ended: Mutex<Vec<RunInfo>>,
}

impl Registry {
  pub fn live(&self) -> Vec<Arc<Run>> {
    lock(&self.live).clone()
  }

  /// The live run `id` names.
  pub fn find(&self, id: &str) -> Result<Arc<Run>, ControlError> {
    pick(id, &lock(&self.live), |s| (&s.id, &s.name))?.ok_or_else(no_such_run)
  }

  /// The run `id` names, for a new command: one being created or
  /// running. A run that finished, stopped or failed is an error that
  /// says so, with its state.
  pub fn find_open(&self, id: &str) -> Result<Arc<Run>, ControlError> {
    let info = match self.find(id) {
      Ok(run) => match run.state.get() {
        RunState::Creating | RunState::Running => return Ok(run),
        _ => run.info(),
      },
      Err(ControlError::NotFound(_)) => self.find_info(id)?,
      Err(e) => return Err(e),
    };
    Err(ControlError::NotFound(format!("run {} has ended: {}", info.id, info.state)))
  }

  /// The live or ended run `id` names; live runs are looked at
  /// first.
  fn find_info(&self, id: &str) -> Result<RunInfo, ControlError> {
    if let Some(run) = pick(id, &lock(&self.live), |s| (&s.id, &s.name))? {
      return Ok(run.info());
    }
    pick(id, &lock(&self.ended), |s| (&s.id, &s.name))?.ok_or_else(no_such_run)
  }

  fn list(&self) -> Vec<RunInfo> {
    let mut runs: Vec<_> = lock(&self.live).iter().map(|s| s.info()).collect();
    runs.extend(lock(&self.ended).iter().cloned());
    runs.sort_by_key(|s| s.created_ms);
    runs
  }

  fn insert(&self, run: Arc<Run>) {
    lock(&self.live).push(run);
  }

  /// Moves run `info.id` from the live runs to the ended ones.
  fn end(&self, info: RunInfo) {
    lock(&self.live).retain(|s| s.id != info.id);
    lock(&self.ended).push(info);
  }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
  // NOTICE: the lists change by one push or retain, so a panic elsewhere
  // leaves them whole.
  mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn no_such_run() -> ControlError {
  ControlError::NotFound("no such run".into())
}

/// The one item `id` names: the one whose ID is `id`, else the only one whose
/// ID starts with `id` or whose name is `id`. An empty `id` names none, and a
/// prefix or name that fits several is an error.
fn pick<T: Clone>(id: &str, items: &[T], names: impl Fn(&T) -> (&str, &str)) -> Result<Option<T>, ControlError> {
  if id.is_empty() {
    return Ok(None);
  }
  if let Some(exact) = items.iter().find(|item| names(item).0 == id) {
    return Ok(Some(exact.clone()));
  }
  let matches: Vec<&T> = items
    .iter()
    .filter(|item| {
      let (item_id, name) = names(item);
      item_id.starts_with(id) || name == id
    })
    .collect();
  match matches.as_slice() {
    [] => Ok(None),
    [one] => Ok(Some((*one).clone())),
    _ => Err(ControlError::Invalid(format!("run {id} is ambiguous: {} runs match", matches.len()))),
  }
}

// ------------------------------------------------------------ lifecycle

impl Daemon {
  /// Creates a run; a failure is kept as a failed run and recorded
  /// as `run.fail`.
  async fn create_run(&self, request: NewRun) -> Result<Arc<Run>> {
    let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
    let created_ms = now_ms();
    // The run's directory is private; the trace's JSONL sink would
    // otherwise create it first.
    let dir = self.run_dir(&id);
    if let Err(e) = needy_api_server::create_private_dir(&dir) {
      tracing::warn!("run {id}: cannot create {}: {e}", dir.display());
    }
    let loaded = load_profile(&id, &request);
    let trace = self.run_trace(&id, loaded.as_ref().ok().map(|profile| &profile.trace));
    let prepared = loaded.and_then(|profile| self.prepare(&id, &request, profile, created_ms, trace.clone()));
    let run = match prepared {
      Ok(prepared) => prepared,
      Err(error) => {
        self.record_failure(&id, &request, created_ms, &trace, &error, Vec::new()).await;
        return Err(error);
      }
    };
    self.runs.insert(run.clone());
    let provisioned = self.provision(&run, &request).await.and_then(|()| unwinding(&run));
    if let Err(error) = provisioned {
      self.rollback(&run, &request, &error).await;
      return Err(error);
    }
    // An actor that exited already has finished its run.
    if let Err(refused) = run.state.advance(RunState::Running) {
      tracing::debug!("run {id}: {refused}");
    }
    // `needy report` right after the create must find the run and its
    // `run.create`.
    run.trace.flush().await;
    Ok(run)
  }

  /// The run before it acquires anything: its directory, profile and
  /// recipe.
  fn prepare(&self, id: &str, request: &NewRun, profile: Profile, created_ms: i64, trace: Dispatch) -> Result<Arc<Run>> {
    let dir = self.run_dir(id);
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    std::fs::write(dir.join("profile.yaml"), &request.profile_yaml)?;
    let or_id = |value: &str| {
      if value.is_empty() {
        id.to_string()
      } else {
        value.to_string()
      }
    };
    let recipe = match &profile.actor.recipe {
      Some(reference) => {
        // The client sends a recipe file's content; the daemon reads only
        // its built-in recipes.
        let source = match Source::reference(reference, Path::new("")) {
          builtin @ Source::Builtin(_) => builtin,
          _ if request.recipe_yaml.is_empty() => bail!("recipe {reference} was not sent with the profile"),
          _ => Source::text(request.recipe_yaml.clone(), PathBuf::new()),
        };
        let recipe = needy_config::recipe::load(&source)?;
        let host = needy_core_actor::host::detect::detect().unwrap_or_else(|e| {
          tracing::warn!("run {id}: cannot read this machine's CPU; the recipe skips host checks: {e}");
          Default::default()
        });
        let seed = u64::from_str_radix(&id[..8], 16).unwrap_or(0);
        let compiled = needy_core::recipe::compile(&recipe, &host, seed);
        for finding in &compiled.findings {
          tracing::warn!("run {id}: recipe {finding}");
        }
        Some(compiled)
      }
      None => None,
    };
    let actor_command = if request.actor_command.is_empty() {
      profile.actor.command.to_vec()
    } else {
      request.actor_command.clone()
    };
    let cancel = self.cancel.child_token();
    Ok(Arc::new(Run {
      id: id.to_string(),
      name: or_id(&request.name),
      group: or_id(&request.group),
      profile_name: request.profile_name.clone(),
      injector: Arc::new(Injector::new(&profile.chaos)),
      profile,
      created_ms,
      dir,
      state: State::new(),
      actor_command,
      recipe,
      trace,
      executors: Mutex::new(BTreeMap::new()),
      released: Mutex::new(Vec::new()),
      actor_exit: Mutex::new(None),
      failure: Mutex::new(None),
      tasks: TaskTracker::new(),
      cancel,
    }))
  }

  /// Acquires the run's executors, copies files in and starts its actor.
  /// A cancelled run token ends it before its next step.
  // NOTICE: a step in progress, such as a provider creating a container or
  // pulling its image, runs to its end: the providers are not safe to drop
  // midway (a container created before the drop would be left behind).
  // TODO(shutdown): pass the run's token into `Provider::create` once
  // the providers can unwind a create themselves, so shutdown need not wait
  // for the step; not needed while each step is bounded (`connect` waits
  // 10 s at most) and a second termination signal forces exit.
  async fn provision(&self, run: &Arc<Run>, request: &NewRun) -> Result<()> {
    if request.start_actor {
      actor::check_host(self)?;
    }
    // Executors present the actor's hostname, so `uname -n` agrees on both sides.
    let hostname = run.recipe.as_ref().and_then(|r| r.hostname.clone());
    for (exec_name, config) in &run.profile.executors {
      let key = config.key.render(&[
        ("run", &run.id),
        ("group", &run.group),
        ("profile", &run.profile_name),
        ("rule", exec_name),
      ]);
      let spec = InstanceSpec {
        name: exec_name.clone(),
        key: format!("{exec_name}@{key}"),
        run: run.id.clone(),
        config: config.clone(),
        base: run.profile.base_dir.clone(),
        user: run.profile.user.clone(),
        hostname: hostname.clone(),
      };
      unwinding(run)?;
      let instance = self.pool.acquire(spec).await.with_context(|| format!("executor {exec_name}"))?;
      run.lock_executors().insert(exec_name.clone(), instance);
    }
    run.emit(&Event::RunCreate(RunCreate {
      name: run.name.clone(),
      group: run.group.clone(),
      profile: run.profile_name.clone(),
      executors: executor_refs(&run.executors()),
    }));
    unwinding(run)?;
    files::copy_in(run).await?;
    if request.start_actor {
      unwinding(run)?;
      actor::start(self, run).await?;
    }
    Ok(())
  }

  /// Unwinds a create that failed after the run was registered.
  async fn rollback(&self, run: &Arc<Run>, request: &NewRun, error: &anyhow::Error) {
    if let Err(refused) = run.state.advance(RunState::Failed) {
      tracing::debug!("run {}: {refused}", run.id);
    }
    // The actor's tasks and VM, if it started, stop.
    run.cancel.cancel();
    run.tasks.close();
    run.tasks.wait().await;
    self.release_executors(run).await;
    actor::remove_sockets(&run.dir);
    let released = lock(&run.released).clone();
    self.record_failure(&run.id, request, run.created_ms, &run.trace, error, released).await;
  }

  /// Keeps a failed create as a failed run, with the executors it
  /// `released`, and records why.
  async fn record_failure(
    &self,
    id: &str,
    request: &NewRun,
    created_ms: i64,
    trace: &Dispatch,
    error: &anyhow::Error,
    released: Vec<ExecutorRef>,
  ) {
    tracing::warn!("run {id} failed: {error:#}");
    let or_id = |value: &str| {
      if value.is_empty() {
        id.to_string()
      } else {
        value.to_string()
      }
    };
    self.runs.end(RunInfo {
      id: id.to_string(),
      name: or_id(&request.name),
      group: or_id(&request.group),
      state: RunState::Failed,
      executors: Vec::new(),
      released,
      created_ms,
      profile: request.profile_name.clone(),
      actor_exit: None,
      failure: Some(format!("{error:#}")),
    });
    emit(
      trace,
      &Event::RunFail(RunFail {
        reason: format!("{error:#}"),
      }),
    );
    // `needy report` right after the failed create must find the reason.
    trace.flush().await;
  }

  /// Gives back what a run whose actor exited holds: it mirrors out,
  /// releases its executors and removes the actor's sockets. The run
  /// stays listed as finished or failed until it is stopped, which then has
  /// nothing left to release.
  pub(crate) async fn finish_run(&self, run: &Run) {
    if let Err(e) = files::mirror_out(run).await {
      tracing::warn!("mirror-out for {}: {e:#}", run.id);
    }
    self.release_executors(run).await;
    actor::remove_sockets(&run.dir);
  }

  /// Stops a running or finished run: its tasks, mirror-out, its
  /// executors, its trace.
  pub(crate) async fn stop_run(&self, run: &Arc<Run>) -> Result<(), ControlError> {
    let stopping = match run.state.advance(RunState::Stopping) {
      Ok(_) => true,
      // The actor finished or failed first and that state stays; ending
      // released its executors already.
      Err(Refused {
        from: RunState::Finished | RunState::Failed,
        ..
      }) => false,
      Err(refused) => return Err(ControlError::Failed(format!("run {}: {refused}", run.id))),
    };
    // The relays of the run's commands and its actor's tasks stop.
    run.cancel.cancel();
    run.tasks.close();
    run.tasks.wait().await;
    if let Err(e) = files::mirror_out(run).await {
      tracing::warn!("mirror-out for {}: {e:#}", run.id);
    }
    if stopping && let Err(refused) = run.state.advance(RunState::Stopped) {
      tracing::warn!("run {}: {refused}", run.id);
    }
    self.release_executors(run).await;
    run.emit(&Event::RunStop(RunStop {}));
    // `needy report` right after the stop must see it, and the timing
    // summary is written now.
    run.trace.flush().await;
    actor::remove_sockets(&run.dir);
    self.runs.end(run.info());
    Ok(())
  }

  /// Ends the processes the run started on its executors, such as
  /// detached commands and a workspace mount's `sftp-server`, then gives
  /// the executors back.
  // TODO(runner-ownership): the control daemon asks each executor here, so a
  // daemon that exits first leaves the processes behind; the runner owns a
  // run's processes and ends them in the design of
  // `docs/ai/references/architecture/2026-11-10-runner-and-run-design.md`
  // ("Ownership and cleanup").
  async fn release_executors(&self, run: &Run) {
    let instances = run.take_executors();
    let request = ListProcessesRequest {
      run_id: run.id.clone(),
      ..Default::default()
    };
    // Instances that share an executor list the same processes.
    let mut asked: Vec<(&'static str, &str)> = Vec::new();
    for instance in instances.values() {
      let executor = (instance.handle.runtime, instance.handle.id.as_str());
      if asked.contains(&executor) {
        continue;
      }
      asked.push(executor);
      let processes = match instance.handle.executor.list_processes(request.clone()).await {
        Ok(list) => list.processes,
        Err(status) => {
          tracing::warn!("run {}: cannot list its processes on executor {}: {}", run.id, instance.name, status.message());
          continue;
        }
      };
      for process in processes.into_iter().filter(|p| p.running) {
        // NOTICE: SIGKILL, as the executors' own shutdown uses: the run
        // is over, and a process that ignores SIGTERM would stay.
        let kill = KillProcessRequest {
          process_id: process.process_id.clone(),
          signal: "SIGKILL".into(),
        };
        if let Err(status) = instance.handle.executor.kill_process(kill).await {
          tracing::warn!("run {}: cannot end process {} on executor {}: {}", run.id, process.process_id, instance.name, status.message());
        }
      }
    }
    for instance in instances.values() {
      self.pool.release(instance).await;
    }
  }

  fn run_dir(&self, id: &str) -> PathBuf {
    self.config.state_dir.join("runs").join(id)
  }

  /// Starts the trace of run `id` with the sinks `config` selects; the
  /// default sinks when the profile could not be read.
  fn run_trace(&self, id: &str, config: Option<&needy_config::profile::Trace>) -> Dispatch {
    let default = needy_config::profile::Trace::default();
    let config = config.unwrap_or(&default);
    let mut sinks: Vec<Box<dyn Sink>> = Vec::new();
    // A sink listed twice is written once.
    let selected: BTreeSet<TraceSink> = config.sinks.iter().copied().collect();
    for sink in selected {
      match sink {
        TraceSink::Sqlite => sinks.push(Box::new(self.traces.sink())),
        TraceSink::Jsonl => {
          let path = self.run_dir(id).join("trace.jsonl");
          match Jsonl::create(&path) {
            Ok(jsonl) => sinks.push(Box::new(jsonl)),
            Err(e) => tracing::warn!("run {id}: the trace goes without JSONL: {e}"),
          }
        }
      }
    }
    Dispatch::new(
      id,
      sinks,
      needy_trace::Options {
        capacity: TRACE_QUEUE,
        version: event::SCHEMA_VERSION,
        timing: config.timing,
      },
    )
  }
}

/// Fails a create whose run's token was cancelled, which only the
/// daemon's shutdown does while a run is created.
fn unwinding(run: &Run) -> Result<()> {
  if run.cancel.is_cancelled() {
    bail!("the daemon shut down while the run was being created");
  }
  Ok(())
}

/// Reads and checks the profile of a run about to be created. The
/// client resolved it, so it names no file to read.
// NOTICE: a profile with relative paths, which a client did not resolve,
// would have them resolve against the daemon's working directory; no base
// directory is set, so they stay relative and fail visibly.
fn load_profile(id: &str, request: &NewRun) -> Result<Profile> {
  let profile = needy_config::profile::load(&Source::text(request.profile_yaml.clone(), PathBuf::new()))?;
  let findings = validate::profile(&profile);
  for finding in findings.iter().filter(|f| f.severity == Severity::Warning) {
    tracing::warn!("run {id}: profile {finding}");
  }
  let errors: Vec<&str> = findings.iter().filter(|f| f.severity == Severity::Error).map(|f| f.message.as_str()).collect();
  if !errors.is_empty() {
    bail!("invalid profile: {}", errors.join("; "));
  }
  Ok(profile)
}

// ------------------------------------------------------------------ API

#[async_trait::async_trait]
impl Runs for Daemon {
  async fn create(&self, request: NewRun) -> Result<RunInfo, ControlError> {
    // Shutdown waits for creates in progress to end or unwind.
    let run = self.creates.track_future(self.create_run(request)).await.map_err(failed)?;
    Ok(run.info())
  }

  async fn get(&self, id: &str) -> Result<RunInfo, ControlError> {
    self.runs.find_info(id)
  }

  async fn list(&self) -> Result<Vec<RunInfo>, ControlError> {
    Ok(self.runs.list())
  }

  async fn stop(&self, id: &str) -> Result<(), ControlError> {
    let run = self.runs.find(id)?;
    self.stop_run(&run).await
  }

  async fn logs(&self, id: &str) -> Result<Vec<u8>, ControlError> {
    let info = self.runs.find_info(id)?;
    // Firecracker writes the guest's serial console here.
    let path = vm::console(&self.run_dir(&info.id));
    // TODO(run-logs): follow a running actor's console (`--follow`); not
    // needed by the QA item, which asks for the output so far.
    match tokio::fs::read(&path).await {
      Ok(bytes) => Ok(bytes),
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(ControlError::NotFound(format!("run {id} has no actor console"))),
      Err(e) => Err(ControlError::Internal(format!("cannot read {}: {e}", path.display()))),
    }
  }

  async fn decide(&self, run: &str, ctx: ExecContext) -> Result<Route, ControlError> {
    let run = self.runs.find(run)?;
    Ok(routing::route(&run, ctx).await)
  }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
