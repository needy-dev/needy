//! Run creation, provisioning, rollback, stopping, and the `Runs` API.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use needy_api_client::proto::v1::{KillProcessRequest, ListProcessesRequest};
use needy_api_server::control::{
  ControlError, FaultAdded, Logs, NewRun, Placement, ProfileCheck, ProfileToCheck, Route, RunEvents, RunFaults, RunInfo, Runs, StopMode,
  StoredEvent,
};
use needy_common::sync::lock;
use needy_common::time::now_ms;
use needy_config::Source;
use needy_config::profile::{Profile, TraceSink};
use needy_config::validate::{self, Severity};
use needy_core::chaos::Injector;
use needy_core::event::{self, ActorStop, Event, RunCreate, RunFail, RunStop};
use needy_core::intercept::exec::ExecContext;
use needy_core::recipe::PerRun;
use needy_core::types::{ActorState, Principal, RunState};
use needy_core_actor::driver;
use needy_core_executor::driver::Spec;
use needy_trace::Dispatch;
use needy_trace::sink::Sink;
use needy_trace::sink::jsonl::Jsonl;
use tokio_util::task::TaskTracker;

use super::logs::{follow_console, load_trace};
use super::registry::no_such_run;
use super::state::{Refused, State};
use super::{Run, emit, executor_refs, route_list};
use crate::pool::{Instance, Site};
use crate::store::{self, InstanceKind, InstanceRecord, InstanceState, RunRecord};
use crate::{Daemon, actor, failed, files, routing, schedule};

/// Events a run's trace queue holds before it drops them.
// NOTICE: a burst larger than this while the sinks are slow is recorded as a
// `trace.gap`; 4096 envelopes are a few MiB at most.
const TRACE_QUEUE: usize = 4096;

impl Daemon {
  /// Creates a run; a failure is kept as a failed run and recorded
  /// as `run.fail`.
  async fn create_run(&self, request: NewRun) -> Result<Arc<Run>> {
    let id = if request.client_execution.is_empty() {
      uuid::Uuid::new_v4().simple().to_string()[..12].to_string()
    } else {
      let invocation = self.state.client_execution_invocation(&request.client_execution).await?;
      self.state.bind_client_execution(&request.client_execution, &invocation, now_ms()).await?;
      invocation
    };
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
    let bound = async {
      let client_selected = run.profile.executors.values().any(|e| matches!(e.runner, Some(needy_config::profile::RunnerSelector::Client)));
      if client_selected {
        if request.client_execution.is_empty() {
          bail!("client execution capability is required");
        }
      } else if !request.client_execution.is_empty() {
        bail!("a client execution capability was supplied but no executor selects the creating client");
      }
      self.provision(&run).await
    }
    .await;
    let provisioned = bound.and_then(|()| unwinding(&run));
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
  pub(crate) fn prepare(&self, id: &str, request: &NewRun, profile: Profile, created_ms: i64, trace: Dispatch) -> Result<Arc<Run>> {
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
        let host = needy_core_actor::driver::linux_firecracker::kvm::detect().unwrap_or_else(|e| {
          tracing::warn!("run {id}: cannot read this machine's CPU; the recipe skips host checks: {e}");
          Default::default()
        });
        let seed = u64::from_str_radix(&id[..8], 16).unwrap_or(0);
        let compiled = needy_core::recipe::compile(&recipe, &host, &PerRun::at(seed, created_ms));
        for finding in &compiled.findings {
          tracing::warn!("run {id}: recipe {finding}");
        }
        Some(compiled)
      }
      None => None,
    };
    // NOTICE: arguments extend the profile's driving program and never
    // replace it (owner decision, 2026-11-12); a profile that `extends`
    // another sets a different `actor.command`.
    let actor_command: Vec<String> = profile.actor.command.iter().chain(&request.actor_args).cloned().collect();
    let cancel = self.cancel.child_token();
    let routes = tokio::sync::watch::Sender::new(route_list(needy_core::rules::route_names(&profile)));
    Ok(Arc::new(Run {
      id: id.to_string(),
      name: or_id(&request.name),
      group: or_id(&request.group),
      profile_name: request.profile_name.clone(),
      injector: Arc::new(Injector::new(&profile.chaos)),
      routes,
      profile,
      profile_yaml: request.profile_yaml.clone(),
      created_ms,
      dir,
      state: State::new(),
      actor_command,
      tty: request.tty,
      wait_for_attach: request.wait_for_attach,
      terminal: tokio::sync::OnceCell::new(),
      actor: Default::default(),
      stop_request: Default::default(),
      stop_complete: Default::default(),
      recipe,
      trace,
      executors: Mutex::new(BTreeMap::new()),
      released: Mutex::new(Vec::new()),
      actor_exit: Mutex::new(None),
      failure: Mutex::new(None),
      actor_runner: Mutex::new(None),
      created_by: request.by.clone(),
      boot: Mutex::default(),
      image_commit: request.image_commit.clone(),
      pinned_features: request.pinned_features.clone(),
      tasks: TaskTracker::new(),
      cancel,
    }))
  }

  /// Places the run's actor and executors on their hosts, acquires the
  /// executors, copies files in and starts the actor.
  /// A cancelled run token ends it before its next step.
  // NOTICE: a step in progress, such as a driver creating a container or
  // pulling its image, runs to its end: the drivers are not safe to drop
  // midway (a container created before the drop would be left behind).
  // TODO(shutdown): pass the run's token into `Driver::create` once
  // the drivers can unwind a create themselves, so shutdown need not wait
  // for the step; not needed while each step is bounded (`connect` waits
  // 10 s at most) and a second termination signal forces exit.
  async fn provision(&self, run: &Arc<Run>) -> Result<()> {
    // Where everything goes is decided before anything is acquired, so a
    // selector that names no runner fails the create at once.
    let runners = self.candidates().await?;
    let actor =
      schedule::placement(run.profile.actor.runner.as_ref(), schedule::Need::Actor(run.profile.actor.driver), &runners).context("actor")?;
    if let Some(refusal) = self.actor_refusal(&actor) {
      bail!("actor: {refusal}");
    }
    *lock(&run.actor_runner) = match &actor {
      Placement::Runner(name) => Some(name.clone()),
      Placement::ControlHost => None,
    };
    actor::check_host(self, run).await?;
    files::check_host_paths(run)?;
    unwinding(run)?;
    crate::image::prepare(self, run).await?;
    // Ensure stores for profile- and feature-declared sync mappings
    // (`<data_dir>/files/<store>`) exist before seeding.
    crate::sync::check(self, run)?;
    let client_holder = self.state.client_execution_for_run(&run.id).await?;
    // NOTICE: this bounds initial helper startup only. A bound executor's
    // disconnection deadline during commands belongs to the exec policy.
    if let Some(holder) = &client_holder
      && self.links.connected(holder, std::time::Duration::from_secs(10)).await.is_none()
    {
      bail!("client execution holder is not connected; no control-host fallback is allowed");
    }
    let mut sites = Vec::new();
    for (exec_name, config) in &run.profile.executors {
      let placement = if matches!(config.runner, Some(needy_config::profile::RunnerSelector::Client)) {
        if config.driver != needy_config::profile::ExecutorDriver::Native {
          bail!("executor {exec_name}: client execution supports native executors; remote container creation is not supported");
        }
        Placement::Runner(client_holder.clone().context("client execution capability is required")?)
      } else {
        schedule::placement(config.runner.as_ref(), schedule::Need::Executor(config.driver), &runners)
          .with_context(|| format!("executor {exec_name}"))?
      };
      let site = self.site(&placement, config.driver).map_err(|e| anyhow::anyhow!("executor {exec_name}: {e}"))?;
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
      let spec = Spec {
        name: exec_name.clone(),
        // A creation capability is an account binding, so a profile/group
        // key cannot share its instance with another creation's holder.
        key: match (&config.runner, &client_holder) {
          (Some(needy_config::profile::RunnerSelector::Client), Some(holder)) => format!("{exec_name}@{key}@{holder}"),
          _ => format!("{exec_name}@{key}"),
        },
        run: run.id.clone(),
        config: config.clone(),
        base: run.profile.base_dir.clone(),
        user: run.profile.user.clone(),
        hostname: hostname.clone(),
      };
      unwinding(run)?;
      let instance = self.pool.acquire(spec, &site).await.with_context(|| format!("executor {exec_name}"))?;
      self.start_instance(&executor_record(run, &instance, &site)).await;
      run.lock_executors().insert(exec_name.clone(), instance);
    }
    run.emit(&Event::RunCreate(RunCreate {
      name: run.name.clone(),
      group: run.group.clone(),
      profile: run.profile_name.clone(),
      executors: executor_refs(&run.executors()),
      by: run.created_by.clone(),
      recipe_time: run.recipe.as_ref().map(|recipe| recipe.time.clone()),
      snapshot: run.boot().snapshot.map(|s| s.digest.to_string()),
    }));
    unwinding(run)?;
    files::copy_in(run).await?;
    unwinding(run)?;
    crate::sync::seed(self, run).await?;
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
    crate::sync::end(self, run).await;
    actor::remove_sockets(self, &run.profile.actor, &run.dir);
    self.runs.end(&run.id);
    self.evict_run_processes(&run.id);
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
      snapshot: None,
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
    crate::sync::end(self, run).await;
    actor::remove_sockets(self, &run.profile.actor, &run.dir);
    self.evict_run_processes(&run.id);
  }

  /// Stops a running or finished run: its actor as `mode` says, then its
  /// tasks, mirror-out, its executors, its trace. `by` is who asked; `None`
  /// when the control plane stops it on its own, as at shutdown. Returns how
  /// the stop ended a running actor.
  /// [`Runs::stop`] enters here; [`Self::complete_stop`] persists the result
  /// and releases resources once, while overlapping callers wait.
  pub(crate) async fn stop_run(&self, run: &Arc<Run>, by: Option<Principal>, mode: StopMode) -> Result<Option<ActorStop>, ControlError> {
    // The caller may disappear at any await, including halfway through
    // releasing an executor. Keep admitted cleanup on the daemon's tracker.
    let daemon = self.clone();
    let run = run.clone();
    self
      .tasks
      .spawn(async move { daemon.stop_owned(&run, by, mode).await })
      .await
      .map_err(|error| ControlError::Failed(format!("stop task failed: {error}")))?
  }

  /// The daemon-owned stop task serializes cleanup with other callers and recovery.
  async fn stop_owned(&self, run: &Arc<Run>, by: Option<Principal>, mode: StopMode) -> Result<Option<ActorStop>, ControlError> {
    // Hold ownership through persistence and executor release, not just the
    // state transition. A waiter may retain an Arc after runs.end removes it.
    // TODO: Escalating an in-progress graceful stop with --now needs separate
    // stop-intent coordination; revisit when the owner requests that slice.
    let mut complete = run.stop_complete.lock().await;
    if *complete {
      return Ok(None);
    }
    let stopping = match run.state.advance(RunState::Stopping) {
      Ok(_) => true,
      Err(Refused {
        from: RunState::Stopped,
        ..
      }) => false,
      // No other stop owns the lock. Continue any accepted cleanup checkpoint
      // instead of reopening work (recovery normally uses resume_stop).
      Err(Refused {
        from: RunState::Stopping,
        ..
      }) => true,
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
    let result = self.complete_stop(run, by, mode, stopping).await;
    *complete = result.is_ok();
    result
  }

  /// Completes a stop admitted by an earlier control instance. Its durable
  /// `stopping` state remains authoritative throughout recovery; this path
  /// never reopens the invocation to work or attempts a running transition.
  pub(crate) async fn resume_stop(&self, run: &Arc<Run>) -> Result<Option<ActorStop>, ControlError> {
    let mut complete = run.stop_complete.lock().await;
    if *complete {
      return Ok(None);
    }
    if run.state.get() != RunState::Stopping {
      return Err(ControlError::Failed(format!("run {} has no accepted stop to resume", run.id)));
    }
    let result = self.complete_stop(run, None, StopMode::Now, true).await;
    *complete = result.is_ok();
    result
  }

  /// The common completion after fresh stop admission or restored intent.
  async fn complete_stop(
    &self,
    run: &Arc<Run>,
    by: Option<Principal>,
    mode: StopMode,
    stopping: bool,
  ) -> Result<Option<ActorStop>, ControlError> {
    let actor = if stopping {
      self.end_actor(run, mode).await
    } else {
      None
    };
    // The relays of the run's commands and its actor's tasks stop, and an
    // actor still running is killed.
    run.cancel.cancel();
    run.tasks.close();
    run.tasks.wait().await;
    files::mirror_out(run).await;
    if stopping && let Err(refused) = run.state.advance(RunState::Stopped) {
      tracing::warn!("run {}: {refused}", run.id);
    }
    self.release_executors(run).await;
    crate::sync::end(self, run).await;
    run.emit(&Event::RunStop(RunStop {
      ended: stopping,
      by,
      actor: actor.clone(),
    }));
    // `needy report` right after the stop must see it, and the timing
    // summary is written now.
    run.trace.flush().await;
    actor::remove_sockets(self, &run.profile.actor, &run.dir);
    self.save_run(run).await;
    self.runs.end(&run.id);
    self.evict_run_processes(&run.id);
    Ok(actor)
  }

  /// Ends the running actor of a run being stopped, as `mode` says, and
  /// returns how; `None` when it had none or it had exited already.
  /// Gracefully, the guest that waits for it on its control stream
  /// (`GuestControl::Stop`) signals the
  /// driving program, mirrors out and ends, which the stop waits for up to
  /// the grace period and [`Config::mirror_out_wait`](crate::Config). An
  /// actor that does not end by then, or whose guest does not wait, is left
  /// to the stop's kill.
  async fn end_actor(&self, run: &Run, mode: StopMode) -> Option<ActorStop> {
    if matches!(run.actor.get(), None | Some(ActorState::Exited)) {
      return None;
    }
    let grace = match mode {
      StopMode::Now => {
        return Some(ActorStop::Forced {
          reason: "asked to stop at once".into(),
        });
      }
      StopMode::Graceful { grace } => grace,
    };
    if !run.stop_request.ask(grace) {
      let reason =
        "the actor's guest was not waiting for a stop: it had not finished starting, or its guest needy is older than graceful stops";
      tracing::warn!("run {}: {reason}; killing the actor", run.id);
      return Some(ActorStop::Forced {
        reason: reason.into(),
      });
    }
    let bound = grace + self.config.mirror_out_wait;
    match tokio::time::timeout(bound, run.actor.exited()).await {
      Ok(()) => Some(ActorStop::Graceful),
      Err(_) => {
        let reason = format!(
          "the actor did not end within {:.1} s (the grace period and {:.1} s for its mirror-out)",
          bound.as_secs_f64(),
          self.config.mirror_out_wait.as_secs_f64()
        );
        tracing::warn!("run {}: {reason}; killing the actor", run.id);
        Some(ActorStop::Forced { reason })
      }
    }
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
      let executor = (instance.handle.driver, instance.handle.id.as_str());
      if asked.contains(&executor) {
        continue;
      }
      asked.push(executor);
      match instance.on_runner() {
        Some(runner) => self.end_on_runner(&run.id, runner).await,
        None => {
          self.end_on_control_host(&run.id, instance).await;
        }
      }
    }
    // Closing this private link makes an online holder reconnect and learn
    // its invocation ended, even when no user processes remain on it.
    match self.state.client_execution_for_run(&run.id).await {
      Ok(Some(holder)) => self.links.close(&holder),
      Ok(None) => {}
      Err(error) => tracing::warn!("run {}: cannot read its client holder for cleanup: {error}", run.id),
    }
    for instance in instances.values() {
      self.advance_instance(&run.id, &store::executor_instance(&run.id, &instance.name), InstanceState::Released, now_ms()).await;
      self.pool.release(instance).await;
    }
  }

  /// Kills the run's processes on an executor of the control host, which
  /// does not know runs: by listing them and killing each. Returns whether
  /// admission closure and every requested process cleanup succeeded.
  pub(crate) async fn end_on_control_host(&self, run: &str, instance: &Instance) -> bool {
    let mut completed = true;
    if instance.handle.driver == "native"
      && let Err(error) = self.holder.end_run(run).await
    {
      tracing::warn!("run {}: cannot close native process admission: {error:#}", run);
      completed = false;
    }
    let request = ListProcessesRequest {
      run_id: run.to_owned(),
      ..Default::default()
    };
    let processes = match instance.handle.executor.list_processes(request).await {
      Ok(list) => list.processes,
      Err(status) => {
        tracing::warn!("run {}: cannot list its processes on executor {}: {}", run, instance.name, status.message());
        return false;
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
          completed = false;
          tracing::warn!("run {}: cannot end process {} on executor {}: {}", run, process.process_id, instance.name, status.message())
        }
      }
    }
    self.record_ended(&processes, &ended).await;
    completed
  }

  /// Writes run `run` as it stands now to `state.db`.
  pub(crate) async fn save_run(&self, run: &Run) {
    if let Err(e) = self.state.save_run(&run.record(now_ms())).await {
      tracing::warn!("run {}: cannot write it to state.db: {e:#}", run.id);
    }
  }

  /// Records in `state.db` a run's new hold on an instance.
  pub(crate) async fn start_instance(&self, record: &InstanceRecord) {
    if let Err(e) = self.state.start_instance(record).await {
      tracing::warn!("run {}: cannot write instance {} to state.db: {e:#}", record.run, record.id);
    }
  }

  pub(crate) fn run_dir(&self, id: &str) -> PathBuf {
    self.config.state_dir.join("runs").join(id)
  }

  /// Starts the trace of run `id` with the sinks `config` selects; the
  /// default sinks when the profile could not be read.
  pub(crate) fn run_trace(&self, id: &str, config: Option<&needy_config::profile::Trace>) -> Dispatch {
    self.run_trace_at(id, config, 0)
  }

  pub(crate) fn run_trace_at(&self, id: &str, config: Option<&needy_config::profile::Trace>, next_seq: u64) -> Dispatch {
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
    Dispatch::resume(
      id,
      sinks,
      needy_trace::Options {
        capacity: TRACE_QUEUE,
        version: event::SCHEMA_VERSION,
        timing: config.timing,
      },
      next_seq,
    )
  }
}

/// The row of `run`'s hold on the executor `instance` created at `site`.
fn executor_record(run: &Run, instance: &Instance, site: &Site) -> InstanceRecord {
  InstanceRecord {
    id: store::executor_instance(&run.id, &instance.name),
    run: run.id.clone(),
    name: instance.name.clone(),
    kind: InstanceKind::Executor,
    runner: match site {
      Site::Runner { name, .. } => Some(name.clone()),
      Site::ControlHost => None,
    },
    driver: instance.handle.driver.to_string(),
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
  let source = Source::text(request.profile_yaml.clone(), PathBuf::new());
  let mut profile = needy_config::profile::load(&source)?;
  crate::workspace::default(&mut profile, &request.client_dir, request.on_control_host, &source)?;
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

#[async_trait::async_trait]
impl Runs for Daemon {
  async fn prepare_client_execution(
    &self,
    host: needy_api_server::control::RunnerHost,
  ) -> Result<needy_api_server::control::ClientExecution, ControlError> {
    self.admit_work()?;
    let (holder, run, capability) = self.state.prepare_client_execution(now_ms()).await.map_err(|e| failed(e.into()))?;
    lock(&self.runner_hosts).insert(holder.clone(), host);
    Ok(needy_api_server::control::ClientExecution {
      run,
      holder,
      capability: capability.into_string(),
    })
  }

  async fn create(&self, request: NewRun) -> Result<RunInfo, ControlError> {
    // Shutdown waits for creates in progress to end or unwind.
    let run = self
      .creates
      .track_future(async {
        self.admit_work()?;
        self.create_run(request).await.map_err(failed)
      })
      .await?;
    Ok(run.info())
  }

  async fn get(&self, id: &str) -> Result<RunInfo, ControlError> {
    self.runs.find_info(id).await
  }

  async fn list(&self) -> Result<Vec<RunInfo>, ControlError> {
    self.runs.list().await
  }

  async fn stop(&self, id: &str, by: Principal, mode: StopMode) -> Result<Option<ActorStop>, ControlError> {
    let run = match self.runs.find(id) {
      Ok(run) => run,
      // A stop removes the run from the live runs, but `run ls` still lists
      // it from `state.db`: stopping it again has nothing left to end.
      Err(ControlError::NotFound(_)) => {
        let info = self.runs.find_info(id).await?;
        if matches!(info.state, RunState::Stopped | RunState::Finished | RunState::Failed) {
          return Ok(None);
        }
        return Err(no_such_run());
      }
      Err(e) => return Err(e),
    };
    self.stop_run(&run, Some(by), mode).await
  }

  async fn logs(&self, id: &str, follow: bool) -> Result<Logs, ControlError> {
    let info = self.runs.find_info(id).await?;
    // The actor's driver writes its console here (Firecracker: the guest's
    // serial console).
    let path = driver::log(&self.run_dir(&info.id));
    let live = self.runs.exact(&info.id).filter(|run| follow && run.state.get() == RunState::Running);
    let Some(run) = live else {
      // The output so far: a run that ended writes nothing more.
      return match tokio::fs::read(&path).await {
        Ok(bytes) => Ok(Box::pin(tokio_stream::iter([Ok(bytes)]))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(
          ControlError::NotFound(format!("run {id} has no actor console")).with_diagnostic("run.no_console", [("run".into(), id.into())]),
        ),
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

  async fn add_fault(&self, run: &str, name: &str, fault_yaml: &str, by: Principal) -> Result<FaultAdded, ControlError> {
    Daemon::add_fault(self, run, name, fault_yaml, by).await
  }

  async fn faults(&self, run: &str) -> Result<RunFaults, ControlError> {
    Daemon::faults(self, run).await
  }

  async fn remove_fault(&self, run: &str, name: &str, by: Principal) -> Result<String, ControlError> {
    Daemon::remove_fault(self, run, name, by).await
  }
}
