//! A live invocation's control snapshot. It is sufficient to reconnect to a
//! verified holder, never evidence that a process survived by itself.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt as _;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use needy_config::profile::OnConflict;
use needy_core::chaos::{FaultState, Injector};
use needy_core::event::ExecutorRef;
use needy_core::image::FeatureSyncFile;
use needy_core::types::{ActorState, RunState};
use serde::{Deserialize, Serialize};
use tokio_util::task::TaskTracker;

use super::{Run, State};
use crate::{Daemon, actor, pool, store};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
  version: u32,
  holder: String,
  command: Vec<String>,
  tty: Option<(u32, u32)>,
  wait_for_attach: bool,
  recipe: Option<needy_core::recipe::CompiledRecipe>,
  actor: Option<ActorState>,
  executors: Vec<pool::Saved>,
  // NOTICE: counters and dynamic faults are checkpointed on graceful control
  // detach. Abrupt crashes retain the last snapshot; process continuity does
  // not promise deterministic recovery of transient policy counters.
  faults: Vec<FaultState>,
  /// Image boot metadata resolved before actor start. Defaulted when reading
  /// snapshots written before boot metadata was persisted.
  #[serde(default)]
  boot: SavedBoot,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedBoot {
  #[serde(default)]
  snapshot_image: Option<String>,
  #[serde(default)]
  path: String,
  #[serde(default)]
  env: BTreeMap<String, String>,
  #[serde(default)]
  commit_rebased: bool,
  #[serde(default)]
  resolved_features: BTreeMap<String, String>,
  #[serde(default)]
  start_hooks: Vec<String>,
  #[serde(default)]
  stop_hooks: Vec<String>,
  #[serde(default)]
  sync_files: Vec<SavedSyncFile>,
}

impl SavedBoot {
  fn from_boot(boot: &super::Boot) -> Self {
    Self {
      snapshot_image: boot.snapshot_image.clone(),
      path: boot.path.clone(),
      env: boot.env.clone(),
      commit_rebased: boot.commit_rebased,
      resolved_features: boot.resolved_features.clone(),
      start_hooks: boot.start_hooks.clone(),
      stop_hooks: boot.stop_hooks.clone(),
      sync_files: boot.sync_files.iter().map(SavedSyncFile::from_feature_sync_file).collect(),
    }
  }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedSyncFile {
  name: String,
  store: String,
  path: String,
  #[serde(default)]
  filter: Vec<String>,
  #[serde(default)]
  on_conflict: Vec<(String, SavedOnConflict)>,
  feature: String,
}

impl SavedSyncFile {
  fn from_feature_sync_file(file: &FeatureSyncFile) -> Self {
    Self {
      name: file.name.clone(),
      store: file.store.clone(),
      path: file.path.clone(),
      filter: file.filter.clone(),
      on_conflict: file.on_conflict.iter().map(|(pattern, choice)| (pattern.clone(), SavedOnConflict::from_on_conflict(choice))).collect(),
      feature: file.feature.clone(),
    }
  }

  fn into_feature_sync_file(self) -> FeatureSyncFile {
    FeatureSyncFile {
      name: self.name,
      store: self.store,
      path: self.path,
      filter: self.filter,
      on_conflict: self.on_conflict.into_iter().map(|(pattern, choice)| (pattern, choice.into_on_conflict())).collect(),
      feature: self.feature,
    }
  }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SavedOnConflict {
  Store,
  Run,
  Lines,
  Driver(String),
}

impl SavedOnConflict {
  fn from_on_conflict(choice: &OnConflict) -> Self {
    match choice {
      OnConflict::Store => Self::Store,
      OnConflict::Run => Self::Run,
      OnConflict::Lines => Self::Lines,
      OnConflict::Driver(command) => Self::Driver(command.clone()),
    }
  }

  fn into_on_conflict(self) -> OnConflict {
    match self {
      Self::Store => OnConflict::Store,
      Self::Run => OnConflict::Run,
      Self::Lines => OnConflict::Lines,
      Self::Driver(command) => OnConflict::Driver(command),
    }
  }
}

/// One restored reference graph. Activation is deferred until every open
/// invocation has restored its pool references, so no watch or pending stop
/// can release a resource while another survivor is still absent from the pool.
pub(crate) struct Restored {
  run: Arc<Run>,
  runtime: Box<dyn needy_core_actor::driver::Running>,
  events: Vec<needy_trace::Event>,
  released: Vec<store::InstanceRecord>,
}

impl Restored {
  /// Enables this invocation's services and watch after all references exist.
  /// Pending stops are returned to the caller and completed only after every
  /// service activation succeeded; a failed startup retains their intent.
  pub(crate) async fn activate(self, daemon: &Daemon) -> Result<Option<Arc<Run>>> {
    let Self {
      run,
      runtime,
      events,
      released,
    } = self;
    for instance in released {
      // A released local container may have outlasted the request that removed
      // it. Reconciliation is idempotent and respects all restored pool holds.
      // Native admission and runner EndRun precede the release checkpoint;
      // neither of those logical references owns a container to destroy here.
      if instance.runner.is_none()
        && instance.driver == needy_config::profile::ExecutorDriver::LinuxContainer.as_str()
        && daemon.end_leftover_executor(&instance).await == store::InstanceState::Lost
      {
        bail!("cannot complete release of executor {} for run {}", instance.name, run.id);
      }
    }
    let channels = actor::channels(daemon, &run)?;
    let token = run.cancel.child_token();
    actor::serve_guest(daemon, &run, &channels, actor::facets(&run).transport, &token).await?;
    actor::serve_gateway(&run, &channels, &token)?;
    actor::resume_services(daemon, &run, &token, &events)?;
    // A configured guest resumes its persistent control stream. The actor
    // process service proves terminal readiness again when next attached.
    let driver = run.profile.actor.driver.as_str();
    run.tasks.spawn(actor::watch(daemon.clone(), run.clone(), driver, runtime, token));
    daemon.save_run(&run).await;
    Ok((run.state.get() == RunState::Stopping).then_some(run))
  }
}

impl Daemon {
  /// Loads the trace events recorded for `run` from `trace.db`, falling back to
  /// `trace.jsonl` when the database has no rows for the run.
  pub(crate) async fn load_run_events(&self, run: &str) -> Result<Vec<needy_trace::Event>> {
    match needy_trace::sink::sqlite::load(&self.config.state_dir.join("trace.db"), run).await {
      Ok(loaded) => Ok(loaded.events),
      Err(needy_trace::Error::NoRun(_) | needy_trace::Error::NoDatabase(_)) => {
        match std::fs::read_to_string(self.run_dir(run).join("trace.jsonl")) {
          Ok(text) => text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(serde_json::from_str)
            .collect::<Result<Vec<needy_trace::Event>, _>>()
            .map_err(Into::into),
          Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
          Err(error) => Err(error.into()),
        }
      }
      Err(error) => Err(error.into()),
    }
  }

  /// Written after acquisition and before actor start, then at graceful
  /// control detach. The directory and snapshot stay private to the owner.
  pub(crate) async fn snapshot_run(&self, run: &Run) -> Result<()> {
    let boot = run.boot();
    let snapshot = Snapshot {
      version: 2,
      holder: self.holder.id().into(),
      command: run.actor_command.clone(),
      tty: run.tty.map(|tty| (tty.rows, tty.cols)),
      wait_for_attach: run.wait_for_attach,
      recipe: run.recipe.clone(),
      actor: run.actor.get(),
      executors: run.executors().values().map(|instance| instance.saved()).collect(),
      faults: run.injector.list(),
      boot: SavedBoot::from_boot(&boot),
    };
    let path = run.dir.join("control-snapshot.json");
    let temporary = run.dir.join(format!(".control-snapshot-{}", uuid::Uuid::new_v4()));
    let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(&temporary)?;
    use std::io::Write as _;
    serde_json::to_writer(&mut file, &snapshot)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    std::fs::rename(temporary, path)?;
    Ok(())
  }

  /// Restores references without enabling services, watches or cleanup.
  /// `None` means the protected holder cannot prove ownership of this
  /// invocation; reconciliation then records actual runtime loss.
  pub(crate) async fn restore_run(&self, record: &store::RunRecord) -> Result<Option<Restored>> {
    let file =
      match OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(self.run_dir(&record.id).join("control-snapshot.json")) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
      };
    let snapshot: Snapshot = serde_json::from_reader(file)?;
    if snapshot.version != 2 {
      bail!("unsupported control snapshot version");
    }
    let runtime = match self.holder.adopt(&record.id, &snapshot.holder).await {
      Ok(runtime) => runtime,
      Err(error) => {
        tracing::warn!("run {}: cannot reconnect its original holder: {error:#}", record.id);
        return Ok(None);
      }
    };
    let profile = needy_config::profile::load(&needy_config::Source::text(record.profile_yaml.clone(), std::path::PathBuf::new()))?;
    let events = self.load_run_events(&record.id).await?;
    let next_seq = events.last().map_or(0, |event| event.seq + 1);
    let trace = self.run_trace_at(&record.id, Some(&profile.trace), next_seq);
    // Instance release is persisted before the external destroy operation and
    // final stopped record. That checkpoint is newer than this snapshot: an
    // interrupted accepted stop must not resurrect or reconnect the reference.
    // Release alone says nothing about mirror-out success; retain its existing
    // trace outcome and let mirror_out skip executors no longer held.
    let ended = if record.state == RunState::Stopping {
      self
        .state
        .instances_of(&record.id)
        .await?
        .into_iter()
        .filter(|instance| matches!(instance.state, store::InstanceState::Released | store::InstanceState::Ended))
        .collect::<Vec<_>>()
    } else {
      Vec::new()
    };
    // Stop releases its actor before its executors. That terminal checkpoint
    // also supersedes the snapshot: resuming before the watch observes the
    // holder's cached exit must not report a second forced actor stop.
    let actor_released = ended.iter().any(|instance| instance.kind == store::InstanceKind::Actor);
    let released = ended.into_iter().filter(|instance| instance.kind == store::InstanceKind::Executor).collect::<Vec<_>>();
    let mut executors = BTreeMap::new();
    for saved in snapshot.executors {
      if released.iter().any(|instance| instance.name == saved.name) {
        continue;
      }
      let config = profile.executors.get(&saved.name).context("a saved executor is absent from its profile")?;
      let instance = self.pool.restore(saved, config).await?;
      executors.insert(instance.name.clone(), instance);
    }
    let elapsed = Duration::from_millis(needy_common::time::now_ms().saturating_sub(record.created_ms).max(0) as u64);
    let created = Instant::now().checked_sub(elapsed).unwrap_or_else(Instant::now);
    let injector = Arc::new(Injector::restore(&profile.chaos, created, snapshot.faults));
    let routes = tokio::sync::watch::Sender::new(super::route_list(needy_core::rules::route_names(&profile)));
    let observed_actor = if actor_released {
      Some(ActorState::Exited)
    } else {
      events
        .iter()
        .rev()
        .find_map(|event| match event.kind.as_str() {
          "actor.ready" => Some(ActorState::Ready),
          "actor.config" => Some(ActorState::Configured),
          "actor.start" => Some(ActorState::Starting),
          _ => None,
        })
        .or(snapshot.actor)
        .or(Some(ActorState::Starting))
    };
    let run = Arc::new(Run {
      id: record.id.clone(),
      name: record.name.clone(),
      group: record.group.clone(),
      profile_name: record.profile_name.clone(),
      profile,
      profile_yaml: record.profile_yaml.clone(),
      created_ms: record.created_ms,
      dir: self.run_dir(&record.id),
      state: State::restore(if record.state == RunState::Creating {
        RunState::Running
      } else {
        record.state
      }),
      actor: actor::state::State::restore(observed_actor),
      stop_request: Default::default(),
      stop_complete: Default::default(),
      actor_command: snapshot.command,
      tty: snapshot.tty.map(|(rows, cols)| needy_api_server::control::WindowSize { rows, cols }),
      terminal: tokio::sync::OnceCell::new(),
      wait_for_attach: snapshot.wait_for_attach,
      recipe: snapshot.recipe,
      injector,
      routes,
      trace,
      executors: Mutex::new(executors),
      released: Mutex::new(
        released
          .iter()
          .map(|instance| ExecutorRef {
            name: instance.name.clone(),
            driver: instance.driver.clone(),
            instance: instance.handle.clone(),
          })
          .collect(),
      ),
      actor_exit: Mutex::new(record.actor_exit.clone()),
      failure: Mutex::new(record.failure.clone()),
      actor_runner: Mutex::new(record.actor_runner.clone()),
      created_by: None,
      image_commit: None,
      pinned_features: Default::default(),
      boot: Mutex::new(super::Boot {
        snapshot: record
          .snapshot
          .as_deref()
          .and_then(|digest| digest.parse().ok())
          .and_then(|digest| self.images.store.resolve(&digest).ok()),
        snapshot_image: snapshot.boot.snapshot_image,
        path: snapshot.boot.path,
        env: snapshot.boot.env,
        commit_rebased: snapshot.boot.commit_rebased,
        resolved_features: snapshot.boot.resolved_features,
        start_hooks: snapshot.boot.start_hooks,
        stop_hooks: snapshot.boot.stop_hooks,
        sync_files: snapshot.boot.sync_files.into_iter().map(SavedSyncFile::into_feature_sync_file).collect(),
        ..super::Boot::default()
      }),
      tasks: TaskTracker::new(),
      cancel: self.cancel.child_token(),
    });
    run.update_routes();
    self.runs.insert(run.clone());
    Ok(Some(Restored {
      run,
      runtime,
      events,
      released,
    }))
  }
}
