//! Runs: create, get, list and stop, through `api-server`'s `Runs`
//! trait, and the registry of live and ended runs.
//!
//! A run is registered while it is created, so it is listed as
//! `creating`. A create that fails unwinds what it did and is kept as a
//! failed run; a stopped run is kept as stopped, so `run ls`,
//! `run logs` and `report` still find it.
//!
//! Every change of a run's state, and of the instances and processes it
//! holds, is written to `state.db` where its trace event is emitted, so
//! ended runs are listed from there, also after a restart, and a restarted
//! control plane reconciles the runs it finds open (`crate::reconcile`).

pub mod state;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context, Result, bail};
use needy_api_client::proto::v1::{KillProcessRequest, ListProcessesRequest};
use needy_api_server::control::{ControlError, Logs, NewRun, ProfileCheck, ProfileToCheck, Route, RunEvents, RunInfo, Runs, StoredEvent};
use needy_common::process::Exit;
use needy_common::time::now_ms;
use needy_config::Source;
use needy_config::profile::{Profile, TraceSink};
use needy_config::validate::{self, Severity};
use needy_core::chaos::Injector;
use needy_core::event::{self, Event, ExecutorRef, RunCreate, RunFail, RunStop};
use needy_core::intercept::ExecContext;
use needy_core::recipe::CompiledRecipe;
use needy_core::types::{Principal, RunState};
use needy_core_actor::host::runtime;
use needy_core_executor::provider::InstanceSpec;
use needy_trace::Dispatch;
use needy_trace::sink::Sink;
use needy_trace::sink::jsonl::Jsonl;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::pool::{Instance, Site};
use crate::store::{self, InstanceKind, InstanceRecord, InstanceState, RunRecord};
use crate::{Daemon, actor, failed, files, routing, schedule};
use needy_api_server::control::Placement;
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
  /// The profile as the client resolved it, kept in `state.db`.
  profile_yaml: String,
  pub created_ms: i64,
  pub dir: PathBuf,
  pub state: State,
  /// The actor's driving program.
  pub actor_command: Vec<String>,
  /// The driving program's terminal size at the start; `None` runs it
  /// without one.
  pub tty: Option<needy_api_server::control::WindowSize>,
  /// The actor's process service for that terminal, once connected.
  pub terminal: tokio::sync::OnceCell<Arc<Instance>>,
  /// The initial interactive client must subscribe before the program starts.
  pub wait_for_attach: bool,
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
  /// The runner its actor was placed on, once placed; `None` for the
  /// control host.
  actor_runner: Mutex<Option<String>>,
  /// Who asked for the run, as `run.create` records it.
  created_by: Option<Principal>,
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

  /// The run as `state.db` keeps it, at `now`.
  fn record(&self, now: i64) -> RunRecord {
    RunRecord {
      id: self.id.clone(),
      name: self.name.clone(),
      group: self.group.clone(),
      profile_name: self.profile_name.clone(),
      profile_yaml: self.profile_yaml.clone(),
      state: self.state.get(),
      created_ms: self.created_ms,
      updated_ms: now,
      actor_exit: self.actor_exit(),
      failure: lock(&self.failure).clone(),
      actor_runner: lock(&self.actor_runner).clone(),
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
      lost: Vec::new(),
      created_ms: self.created_ms,
      profile: self.profile_name.clone(),
      // NOTICE: an actor whose VM ended before init reported the driving
      // program's exit has none; `actor.vm.exit` in the trace has the VM's.
      actor_exit: lock(&self.actor_exit).clone(),
      failure: lock(&self.failure).clone(),
      tty: self.tty.is_some(),
    }
  }

  fn lock_executors(&self) -> MutexGuard<'_, BTreeMap<String, Arc<Instance>>> {
    // NOTICE: each change is one insert, so a panic elsewhere leaves the map
    // whole.
    self.executors.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }
}

/// How often a followed console is read again for what the actor wrote.
// NOTICE: polling, not a file watcher: the console is one file that
// Firecracker appends to, and a quarter second is as quick as a person
// reading `run logs -f` needs; a watcher (`notify`) would add a dependency
// and a platform backend for no visible gain.
const FOLLOW_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// The console at `path` from its start, then what the actor writes
/// next, until `run` is no longer running or the caller leaves.
fn follow_console(run: Arc<Run>, path: PathBuf) -> Logs {
  use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
  let (tx, rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, ControlError>>(16);
  let cancel = run.cancel.clone();
  let tasks = run.tasks.clone();
  tasks.spawn(async move {
    let mut offset = 0u64;
    loop {
      // The state is read before the file, so the read after the run
      // stopped running is the last and catches everything it wrote.
      let running = run.state.get() == RunState::Running && !cancel.is_cancelled();
      let read = async {
        let mut file = tokio::fs::File::open(&path).await?;
        file.seek(std::io::SeekFrom::Start(offset)).await?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).await?;
        Ok::<_, std::io::Error>(bytes)
      };
      match read.await {
        Ok(bytes) if !bytes.is_empty() => {
          offset += bytes.len() as u64;
          if tx.send(Ok(bytes)).await.is_err() {
            return;
          }
        }
        Ok(_) => {}
        // The VM has not created its console yet.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
          // The caller may be gone; there is nobody else to tell.
          let _ = tx.send(Err(ControlError::Internal(format!("cannot read {}: {e}", path.display())))).await;
          return;
        }
      }
      if !running {
        return;
      }
      tokio::select! {
        () = tokio::time::sleep(FOLLOW_POLL) => {}
        () = cancel.cancelled() => {}
        () = tx.closed() => return,
      }
    }
  });
  Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
}

/// The trace events of `run` in `trace.db`, as a control error.
async fn load_trace(db: &Path, run: &str) -> Result<needy_trace::sink::sqlite::Loaded, ControlError> {
  needy_trace::sink::sqlite::load(db, run).await.map_err(|e| match e {
    needy_trace::Error::NoRun(_) | needy_trace::Error::NoDatabase(_) => ControlError::NotFound(format!("no trace of a run matching {run}")),
    e => ControlError::Internal(format!("{}: {e}", db.display())),
  })
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

/// Live runs in the order they were created; ended runs are read from
/// `state.db`.
pub struct Registry {
  live: Mutex<Vec<Arc<Run>>>,
  store: store::Store,
}

impl Registry {
  pub fn new(store: store::Store) -> Self {
    Self {
      live: Mutex::default(),
      store,
    }
  }

  pub fn live(&self) -> Vec<Arc<Run>> {
    lock(&self.live).clone()
  }

  /// The live run `id` names.
  pub fn find(&self, id: &str) -> Result<Arc<Run>, ControlError> {
    pick(id, &lock(&self.live), |s| (&s.id, &s.name))?.ok_or_else(no_such_run)
  }

  /// The live run whose ID is exactly `id`.
  pub fn exact(&self, id: &str) -> Option<Arc<Run>> {
    lock(&self.live).iter().find(|run| run.id == id).cloned()
  }

  /// The run `id` names, for a new command: one being created or
  /// running. A run that finished, stopped or failed is an error that
  /// says so, with its state.
  pub async fn find_open(&self, id: &str) -> Result<Arc<Run>, ControlError> {
    let info = match self.find(id) {
      Ok(run) => match run.state.get() {
        RunState::Creating | RunState::Running => return Ok(run),
        _ => run.info(),
      },
      Err(ControlError::NotFound(_)) => self.find_info(id).await?,
      Err(e) => return Err(e),
    };
    Err(ControlError::NotFound(format!("run {} has ended: {}", info.id, info.state)))
  }

  /// The live or ended run `id` names; live runs are looked at
  /// first.
  async fn find_info(&self, id: &str) -> Result<RunInfo, ControlError> {
    if let Some(run) = pick(id, &lock(&self.live), |s| (&s.id, &s.name))? {
      return Ok(run.info());
    }
    let record = match self.store.run(id).await.map_err(stored)? {
      Some(exact) => exact,
      None => {
        let stored = self.store.runs().await.map_err(stored)?;
        pick(id, &stored, |r| (&r.id, &r.name))?.ok_or_else(no_such_run)?
      }
    };
    self.stored_info(record).await
  }

  /// Every run, live and ended, oldest first.
  async fn list(&self) -> Result<Vec<RunInfo>, ControlError> {
    let live = self.live();
    let mut runs: Vec<RunInfo> = live.iter().map(|run| run.info()).collect();
    for record in self.store.runs().await.map_err(stored)? {
      if !live.iter().any(|run| run.id == record.id) {
        runs.push(self.stored_info(record).await?);
      }
    }
    runs.sort_by_key(|s| s.created_ms);
    Ok(runs)
  }

  /// An ended run as `state.db` has it, with its executors by the state of
  /// its hold on them.
  async fn stored_info(&self, record: RunRecord) -> Result<RunInfo, ControlError> {
    let instances = self.store.instances_of(&record.id).await.map_err(stored)?;
    let (mut executors, mut released, mut lost) = (Vec::new(), Vec::new(), Vec::new());
    for instance in instances.into_iter().filter(|i| i.kind == InstanceKind::Executor) {
      let reference = ExecutorRef {
        name: instance.name,
        runtime: instance.runtime,
        instance: instance.handle,
      };
      match instance.state {
        InstanceState::Running => executors.push(reference),
        InstanceState::Released | InstanceState::Ended => released.push(reference),
        InstanceState::Lost => lost.push(reference),
      }
    }
    Ok(RunInfo {
      id: record.id,
      name: record.name,
      group: record.group,
      state: record.state,
      executors,
      released,
      lost,
      created_ms: record.created_ms,
      profile: record.profile_name,
      actor_exit: record.actor_exit,
      failure: record.failure,
      // NOTICE: `state.db` does not keep whether the driving program had a
      // terminal; an ended run cannot be attached anyway.
      tty: false,
    })
  }

  fn insert(&self, run: Arc<Run>) {
    lock(&self.live).push(run);
  }

  /// Removes run `id` from the live runs; `state.db` keeps it.
  fn end(&self, id: &str) {
    lock(&self.live).retain(|s| s.id != id);
  }
}

/// A failure to read `state.db`, for the caller.
fn stored(error: store::Error) -> ControlError {
  tracing::error!("state.db: {error:#}");
  ControlError::Internal(format!("cannot read the runs: {error}"))
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
        self.record_failure(&id, &request, created_ms, &trace, &error, None).await;
        return Err(error);
      }
    };
    self.runs.insert(run.clone());
    self.save_run(&run).await;
    let provisioned = self.provision(&run).await.and_then(|()| unwinding(&run));
    if let Err(error) = provisioned {
      self.rollback(&run, &request, &error).await;
      return Err(error);
    }
    // An actor that exited already has finished its run.
    if let Err(refused) = run.state.advance(RunState::Running) {
      tracing::debug!("run {id}: {refused}");
    }
    self.save_run(&run).await;
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
      profile_yaml: request.profile_yaml.clone(),
      created_ms,
      dir,
      state: State::new(),
      actor_command,
      tty: request.tty,
      wait_for_attach: request.wait_for_attach,
      terminal: tokio::sync::OnceCell::new(),
      recipe,
      trace,
      executors: Mutex::new(BTreeMap::new()),
      released: Mutex::new(Vec::new()),
      actor_exit: Mutex::new(None),
      failure: Mutex::new(None),
      actor_runner: Mutex::new(None),
      created_by: request.by.clone(),
      tasks: TaskTracker::new(),
      cancel,
    }))
  }

  /// Places the run's actor and executors on their hosts, acquires the
  /// executors, copies files in and starts the actor.
  /// A cancelled run token ends it before its next step.
  // NOTICE: a step in progress, such as a provider creating a container or
  // pulling its image, runs to its end: the providers are not safe to drop
  // midway (a container created before the drop would be left behind).
  // TODO(shutdown): pass the run's token into `Provider::create` once
  // the providers can unwind a create themselves, so shutdown need not wait
  // for the step; not needed while each step is bounded (`connect` waits
  // 10 s at most) and a second termination signal forces exit.
  async fn provision(&self, run: &Arc<Run>) -> Result<()> {
    // Where everything goes is decided before anything is acquired, so a
    // selector that names no runner fails the create at once.
    let runners = self.candidates().await?;
    let actor =
      schedule::placement(run.profile.actor.runner.as_ref(), schedule::Need::actor(run.profile.actor.runtime), &runners).context("actor")?;
    if let Some(refusal) = self.actor_refusal(&actor) {
      bail!("actor: {refusal}");
    }
    *lock(&run.actor_runner) = match &actor {
      Placement::Runner(name) => Some(name.clone()),
      Placement::ControlHost => None,
    };
    actor::check_host(self, run).await?;
    files::check_host_paths(run)?;
    let mut sites = Vec::new();
    for (exec_name, config) in &run.profile.executors {
      let placement = schedule::placement(config.runner.as_ref(), schedule::Need::executor(config.provider), &runners)
        .with_context(|| format!("executor {exec_name}"))?;
      let site = self.site(&placement, config.provider).map_err(|e| anyhow::anyhow!("executor {exec_name}: {e}"))?;
      sites.push(site);
    }
    // Executors present the actor's hostname, so `uname -n` agrees on both sides.
    let hostname = run.recipe.as_ref().and_then(|r| r.hostname.clone());
    for ((exec_name, config), site) in run.profile.executors.iter().zip(sites) {
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
      let instance = self.pool.acquire(spec, &site).await.with_context(|| format!("executor {exec_name}"))?;
      self.save_instance(&executor_record(run, &instance, &site, config.provider)).await;
      run.lock_executors().insert(exec_name.clone(), instance);
    }
    run.emit(&Event::RunCreate(RunCreate {
      name: run.name.clone(),
      group: run.group.clone(),
      profile: run.profile_name.clone(),
      executors: executor_refs(&run.executors()),
      by: run.created_by.clone(),
    }));
    unwinding(run)?;
    files::copy_in(run).await?;
    unwinding(run)?;
    actor::start(self, run).await?;
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
    actor::remove_sockets(self, &run.profile.actor, &run.dir);
    self.runs.end(&run.id);
    let actor_runner = lock(&run.actor_runner).clone();
    self.record_failure(&run.id, request, run.created_ms, &run.trace, error, actor_runner).await;
  }

  /// Keeps a failed create as a failed run in `state.db`, and records
  /// why.
  async fn record_failure(
    &self,
    id: &str,
    request: &NewRun,
    created_ms: i64,
    trace: &Dispatch,
    error: &anyhow::Error,
    actor_runner: Option<String>,
  ) {
    tracing::warn!("run {id} failed: {error:#}");
    let or_id = |value: &str| {
      if value.is_empty() {
        id.to_string()
      } else {
        value.to_string()
      }
    };
    let record = RunRecord {
      id: id.to_string(),
      name: or_id(&request.name),
      group: or_id(&request.group),
      profile_name: request.profile_name.clone(),
      profile_yaml: request.profile_yaml.clone(),
      state: RunState::Failed,
      created_ms,
      updated_ms: now_ms(),
      actor_exit: None,
      failure: Some(format!("{error:#}")),
      actor_runner,
    };
    if let Err(e) = self.state.save_run(&record).await {
      tracing::warn!("run {id}: cannot write it to state.db: {e:#}");
    }
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
    self.save_run(run).await;
    files::mirror_out(run).await;
    self.release_executors(run).await;
    actor::remove_sockets(self, &run.profile.actor, &run.dir);
  }

  /// Stops a running or finished run: its tasks, mirror-out, its
  /// executors, its trace. `by` is who asked; `None` when the control plane
  /// stops it on its own, as at shutdown.
  pub(crate) async fn stop_run(&self, run: &Arc<Run>, by: Option<Principal>) -> Result<(), ControlError> {
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
    if stopping {
      self.save_run(run).await;
    }
    // The relays of the run's commands and its actor's tasks stop.
    run.cancel.cancel();
    run.tasks.close();
    run.tasks.wait().await;
    files::mirror_out(run).await;
    if stopping && let Err(refused) = run.state.advance(RunState::Stopped) {
      tracing::warn!("run {}: {refused}", run.id);
    }
    self.release_executors(run).await;
    run.emit(&Event::RunStop(RunStop { by }));
    // `needy report` right after the stop must see it, and the timing
    // summary is written now.
    run.trace.flush().await;
    actor::remove_sockets(self, &run.profile.actor, &run.dir);
    self.save_run(run).await;
    self.runs.end(&run.id);
    Ok(())
  }

  /// Ends the processes the run started on its executors, such as
  /// detached commands and a workspace mount's `sftp-server`, then gives
  /// the executors back. A runner ends the run's processes itself when told
  /// the run ended (`EndRun`); the control host's executors are asked
  /// process by process.
  async fn release_executors(&self, run: &Run) {
    let instances = run.take_executors();
    // Instances that share an executor run the same processes.
    let mut asked: Vec<(&'static str, &str)> = Vec::new();
    for instance in instances.values() {
      let executor = (instance.handle.runtime, instance.handle.id.as_str());
      if asked.contains(&executor) {
        continue;
      }
      asked.push(executor);
      match instance.on_runner() {
        Some(runner) => self.end_on_runner(&run.id, runner).await,
        None => self.end_on_control_host(run, instance).await,
      }
    }
    for instance in instances.values() {
      let row = store::executor_instance(&run.id, &instance.name);
      if let Err(e) = self.state.set_instance_state(&row, InstanceState::Released, now_ms()).await {
        tracing::warn!("run {}: cannot write the release of executor {} to state.db: {e:#}", run.id, instance.name);
      }
      self.pool.release(instance).await;
    }
  }

  /// Kills the run's processes on an executor of the control host, which
  /// does not know runs: by listing them and killing each.
  async fn end_on_control_host(&self, run: &Run, instance: &Instance) {
    let request = ListProcessesRequest {
      run_id: run.id.clone(),
      ..Default::default()
    };
    let processes = match instance.handle.executor.list_processes(request).await {
      Ok(list) => list.processes,
      Err(status) => {
        tracing::warn!("run {}: cannot list its processes on executor {}: {}", run.id, instance.name, status.message());
        return;
      }
    };
    let mut ended = Vec::new();
    for process in processes.iter().filter(|p| p.running) {
      // NOTICE: SIGKILL, as the executors' own shutdown uses: the run
      // is over, and a process that ignores SIGTERM would stay.
      let kill = KillProcessRequest {
        process_id: process.process_id.clone(),
        signal: "SIGKILL".into(),
      };
      match instance.handle.executor.kill_process(kill).await {
        Ok(()) => ended.push(process.process_id.clone()),
        Err(status) => {
          tracing::warn!("run {}: cannot end process {} on executor {}: {}", run.id, process.process_id, instance.name, status.message())
        }
      }
    }
    self.record_ended(&processes, &ended).await;
  }

  /// Writes run `run` as it stands now to `state.db`.
  pub(crate) async fn save_run(&self, run: &Run) {
    if let Err(e) = self.state.save_run(&run.record(now_ms())).await {
      tracing::warn!("run {}: cannot write it to state.db: {e:#}", run.id);
    }
  }

  /// Writes a run's hold on an instance to `state.db`.
  pub(crate) async fn save_instance(&self, record: &InstanceRecord) {
    if let Err(e) = self.state.save_instance(record).await {
      tracing::warn!("run {}: cannot write instance {} to state.db: {e:#}", record.run, record.id);
    }
  }

  pub(crate) fn run_dir(&self, id: &str) -> PathBuf {
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

/// The row of `run`'s hold on the executor `instance` created at `site`.
fn executor_record(run: &Run, instance: &Instance, site: &Site, provider: needy_config::profile::Provider) -> InstanceRecord {
  InstanceRecord {
    id: store::executor_instance(&run.id, &instance.name),
    run: run.id.clone(),
    name: instance.name.clone(),
    kind: InstanceKind::Executor,
    runner: match site {
      Site::Runner { name, .. } => Some(name.clone()),
      Site::ControlHost => None,
    },
    provider: format!("{provider:?}").to_lowercase(),
    runtime: instance.handle.runtime.to_string(),
    handle: instance.handle.id.clone(),
    pid: None,
    state: InstanceState::Running,
    created_ms: now_ms(),
    ended_ms: None,
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
    self.runs.find_info(id).await
  }

  async fn list(&self) -> Result<Vec<RunInfo>, ControlError> {
    self.runs.list().await
  }

  async fn stop(&self, id: &str, by: Principal) -> Result<(), ControlError> {
    let run = self.runs.find(id)?;
    self.stop_run(&run, Some(by)).await
  }

  async fn logs(&self, id: &str, follow: bool) -> Result<Logs, ControlError> {
    let info = self.runs.find_info(id).await?;
    // The actor's runtime writes its console here (Firecracker: the guest's
    // serial console).
    let path = runtime::log(&self.run_dir(&info.id));
    let live = self.runs.exact(&info.id).filter(|run| follow && run.state.get() == RunState::Running);
    let Some(run) = live else {
      // The output so far: a run that ended writes nothing more.
      return match tokio::fs::read(&path).await {
        Ok(bytes) => Ok(Box::pin(tokio_stream::iter([Ok(bytes)]))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(ControlError::NotFound(format!("run {id} has no actor console"))),
        Err(e) => Err(ControlError::Internal(format!("cannot read {}: {e}", path.display()))),
      };
    };
    Ok(follow_console(run, path))
  }

  async fn events(&self, run: &str) -> Result<RunEvents, ControlError> {
    let db = self.config.state_dir.join("trace.db");
    let mut loaded = load_trace(&db, run).await?;
    // A live run's last events may still be queued for the sinks.
    if let Some(live) = self.runs.exact(&loaded.run) {
      live.trace.flush().await;
      loaded = load_trace(&db, &loaded.run).await?;
    }
    let events = loaded
      .events
      .into_iter()
      .map(|event| StoredEvent {
        seq: event.seq,
        time_ms: event.time_ms,
        kind: event.kind,
        version: event.version,
        payload: event.payload.get().to_string(),
      })
      .collect();
    Ok(RunEvents {
      run: loaded.run,
      events,
    })
  }

  async fn decide(&self, run: &str, ctx: ExecContext) -> Result<Route, ControlError> {
    let run = self.runs.find(run)?;
    Ok(routing::route(&run, ctx).await)
  }

  async fn check(&self, profile: ProfileToCheck) -> Result<ProfileCheck, ControlError> {
    self.check_profile(profile).await
  }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
