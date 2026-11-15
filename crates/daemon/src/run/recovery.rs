//! A live invocation's control snapshot. It is sufficient to reconnect to a
//! verified holder, never evidence that a process survived by itself.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt as _;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use needy_core::chaos::{FaultState, Injector};
use needy_core::event::ExecutorRef;
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
  /// Written after acquisition and before actor start, then at graceful
  /// control detach. The directory and snapshot stay private to the owner.
  pub(crate) async fn snapshot_run(&self, run: &Run) -> Result<()> {
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
    let events = match needy_trace::sink::sqlite::load(&self.config.state_dir.join("trace.db"), &record.id).await {
      Ok(loaded) => loaded.events,
      Err(needy_trace::Error::NoRun(_) | needy_trace::Error::NoDatabase(_)) => {
        match std::fs::read_to_string(self.run_dir(&record.id).join("trace.jsonl")) {
          Ok(text) => {
            text.lines().filter(|line| !line.trim().is_empty()).map(serde_json::from_str).collect::<Result<Vec<needy_trace::Event>, _>>()?
          }
          Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
          Err(error) => return Err(error.into()),
        }
      }
      Err(error) => return Err(error.into()),
    };
    let next_seq = events.last().map_or(0, |event| event.seq + 1);
    let trace = self.run_trace_at(&record.id, Some(&profile.trace), next_seq);
    // Instance release is persisted before the external destroy operation and
    // final stopped record. That checkpoint is newer than this snapshot: an
    // interrupted accepted stop must not resurrect or reconnect the reference.
    // Release alone says nothing about mirror-out success; retain its existing
    // trace outcome and let mirror_out skip executors no longer held.
    let released = if record.state == RunState::Stopping {
      self
        .state
        .instances_of(&record.id)
        .await?
        .into_iter()
        .filter(|instance| {
          instance.kind == store::InstanceKind::Executor
            && matches!(instance.state, store::InstanceState::Released | store::InstanceState::Ended)
        })
        .collect::<Vec<_>>()
    } else {
      Vec::new()
    };
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
    let observed_actor = events
      .iter()
      .rev()
      .find_map(|event| match event.kind.as_str() {
        "actor.ready" => Some(ActorState::Ready),
        "actor.config" => Some(ActorState::Configured),
        "actor.start" => Some(ActorState::Starting),
        _ => None,
      })
      .or(snapshot.actor)
      .or(Some(ActorState::Starting));
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
