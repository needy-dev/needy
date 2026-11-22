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

mod lifecycle;
mod logs;
mod recovery;
mod registry;
pub mod state;
pub mod stop;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use needy_api_server::control::{GuestSession, RunInfo};
use needy_common::process::Exit;
use needy_common::sync::lock;
use needy_config::profile::{FaultDomain, Profile};
use needy_core::chaos::Injector;
use needy_core::event::{Event, ExecutorRef, RunFail};
use needy_core::recipe::CompiledRecipe;
use needy_core::types::{Principal, RunState};
use needy_core_actor::image::store::{LayerDisk, Snapshot};
use needy_trace::Dispatch;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

pub(crate) use self::registry::Registry;
use crate::actor;
use crate::pool::Instance;
use crate::store::RunRecord;
use state::State;

/// How long a graceful stop waits, after the driving program's grace
/// period, for the actor to mirror out, report the program's exit and end.
// NOTICE: a mirror-out is packed in the guest and unpacked on the control
// host; a minute covers an agent's state directory of hundreds of MiB, and
// an actor still running then is killed without it.
pub const MIRROR_OUT_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// A live run: being created, running, finished or stopping.
pub struct Run {
  pub id: String,
  pub name: String,
  pub group: String,
  pub profile_name: String,
  pub profile: Profile,
  /// The profile as the client resolved it, kept in `state.db`.
  pub(crate) profile_yaml: String,
  pub created_ms: i64,
  pub dir: PathBuf,
  pub state: State,
  /// Its actor's lifecycle; `None` until the actor starts.
  pub actor: actor::state::State,
  /// A graceful stop its actor's guest waits for.
  pub stop_request: stop::StopRequest,
  /// Serializes cleanup and records completion independently of terminal state.
  pub(super) stop_complete: tokio::sync::Mutex<bool>,
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
  /// The actor's route list as init is told it (`*` for every exec): set
  /// from the profile, and again whenever `needy chaos add` or `rm` changes
  /// the run's exec faults ([`Run::update_routes`]); `Guest::control`
  /// streams its changes to the actor.
  pub routes: tokio::sync::watch::Sender<Vec<String>>,
  pub trace: Dispatch,
  /// The run's executors by name, added as they are acquired.
  pub(super) executors: Mutex<BTreeMap<String, Arc<Instance>>>,
  /// The executors it gave back, for `run ls`.
  pub(super) released: Mutex<Vec<ExecutorRef>>,
  /// How the actor's driving program ended, as its init reported it.
  pub(super) actor_exit: Mutex<Option<Exit>>,
  /// Why the run failed after it was created, such as an actor whose VM
  /// exited before its driving program did.
  pub(super) failure: Mutex<Option<String>>,
  /// The runner its actor was placed on, once placed; `None` for the
  /// control host.
  pub(super) actor_runner: Mutex<Option<String>>,
  /// Who asked for the run, as `run.create` records it.
  pub(super) created_by: Option<Principal>,
  /// What its actor boots of its image, as `Guest::config` tells init.
  pub(super) boot: Mutex<Boot>,
  /// Optional commit layer from the image's lockfile, passed by the CLI.
  pub image_commit: Option<needy_core::image::Commit>,
  /// Pinned feature digests (`canonical_ref -> sha256:<hex>`) from the
  /// image's lockfile, passed by the CLI.
  pub pinned_features: BTreeMap<String, String>,
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
  pub(super) fn take_executors(&self) -> BTreeMap<String, Arc<Instance>> {
    let taken = std::mem::take(&mut *self.lock_executors());
    lock(&self.released).extend(executor_refs(&taken));
    taken
  }

  /// The executable names the actor routes now: the profile's rules and
  /// the run's exec faults as they are now; `None` routes every exec.
  pub fn route_names(&self) -> Option<Vec<String>> {
    let faults = self.injector.faults(FaultDomain::Exec);
    needy_core::rules::route_names_for(&self.profile, faults.iter().filter_map(|(_, fault)| fault.exec.as_deref()))
  }

  /// Sets [`Run::routes`] from the run's exec faults now, after `needy
  /// chaos add` or `rm`; a running actor's init is told when it changed.
  pub fn update_routes(&self) {
    let names = route_list(self.route_names());
    self.routes.send_if_modified(|routes| {
      let changed = *routes != names;
      *routes = names;
      changed
    });
  }

  /// What the run's actor boots of its image.
  pub fn boot(&self) -> Boot {
    lock(&self.boot).clone()
  }

  /// Sets what the run's actor boots, before the actor asks for its
  /// configuration.
  pub(crate) fn set_boot(&self, boot: Boot) {
    *lock(&self.boot) = boot;
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
  pub(super) fn record(&self, now: i64) -> RunRecord {
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
      snapshot: self.boot().snapshot.map(|s| s.digest.to_string()),
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
      actor: self.actor.get(),
      workspace: crate::workspace::describe(&self.profile),
      snapshot: self.boot().snapshot.map(|s| s.digest.to_string()),
      commit_rebased: self.boot().commit_rebased,
      resolved_features: self.boot().resolved_features,
    }
  }

  pub(super) fn lock_executors(&self) -> MutexGuard<'_, BTreeMap<String, Arc<Instance>>> {
    lock(&self.executors)
  }
}

pub fn emit(trace: &Dispatch, event: &Event) {
  trace.emit(event.kind().as_str(), event.raw());
}

pub(super) fn executor_refs(executors: &BTreeMap<String, Arc<Instance>>) -> Vec<ExecutorRef> {
  executors
    .iter()
    .map(|(name, instance)| ExecutorRef {
      name: name.clone(),
      driver: instance.handle.driver.into(),
      instance: instance.handle.id.clone(),
    })
    .collect()
}

/// What a run's actor boots of its image: the snapshot's layers over needy's
/// base, the materialized layer disks for the VM drivers, the image's `PATH`,
/// its `env` (without `PATH`), and the session the actor is. The default
/// boots the base alone, as a run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Boot {
  /// `None` boots the base alone.
  pub snapshot: Option<Snapshot>,
  /// The materialized layer disks of `snapshot`, lowest first
  /// (`needy_core_actor::image::store::Store::materialize`), passed to a VM
  /// driver's `Start::layer_disks`. Empty for container drivers.
  pub layer_disks: Vec<LayerDisk>,
  /// The assembled OCI image tag of `snapshot` loaded into the container
  /// engine or Apple's store (`needy_core_actor::image::store::snapshot_tag`),
  /// passed to a container driver's `Start::snapshot_image`. `None` for VM
  /// drivers or runs without an image definition.
  pub snapshot_image: Option<String>,
  /// The image's `PATH` (`needy_core::image::Plan::path`); empty for none.
  pub path: String,
  /// The image's environment (`needy_core::image::Plan::env`), without `PATH`;
  /// `profile.actor.env` overrides it.
  pub env: BTreeMap<String, String>,
  pub session: GuestSession,
  /// True when the lockfile's commit layer was applied on a changed base.
  pub commit_rebased: bool,
  /// Resolved feature digests (`canonical_ref -> sha256:<hex>`) for the CLI
  /// to record in the image's lockfile.
  pub resolved_features: BTreeMap<String, String>,
  /// Lifecycle `start` hooks (`needy_core::image::Plan::hooks.start`).
  pub start_hooks: Vec<String>,
  /// Lifecycle `stop` hooks (`needy_core::image::Plan::hooks.stop`).
  pub stop_hooks: Vec<String>,
  /// Sync mappings declared by features (`customizations.needy.files`) in
  /// resolved layer order, backed by `<daemon.data_dir>/files/<store>`.
  pub sync_files: Vec<needy_core::image::FeatureSyncFile>,
}

/// A route list as init is told it: the names, or `*` for every exec.
pub fn route_list(names: Option<Vec<String>>) -> Vec<String> {
  names.unwrap_or_else(|| vec!["*".into()])
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
