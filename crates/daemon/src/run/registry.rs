//! The registry of live runs and lookup over live and persisted runs.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use needy_api_server::control::{ControlError, RunInfo};
use needy_common::sync::lock;
use needy_core::event::ExecutorRef;
use needy_core::types::RunState;

use super::Run;
use crate::store::{self, InstanceKind, InstanceState, RunRecord};

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
    Err(ControlError::NotFound(format!("run {} has ended: {}", info.id, info.state)).with_diagnostic(
      "run.ended",
      [
        ("run".into(), info.id),
        ("state".into(), info.state.to_string()),
      ],
    ))
  }

  /// The live or ended run `id` names; live runs are looked at
  /// first.
  pub(super) async fn find_info(&self, id: &str) -> Result<RunInfo, ControlError> {
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
  pub(super) async fn list(&self) -> Result<Vec<RunInfo>, ControlError> {
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
        driver: instance.driver,
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
      // NOTICE: `state.db` does not keep the actor's state either: an ended
      // run's actor exited, and a reconciled run's was lost.
      actor: None,
      // NOTICE: nor its workspace, which a client reads when it creates
      // the run.
      workspace: None,
      snapshot: record.snapshot,
      commit_rebased: false,
      resolved_features: BTreeMap::new(),
    })
  }

  pub(crate) fn insert(&self, run: Arc<Run>) {
    lock(&self.live).push(run);
  }

  /// Removes run `id` from the live runs; `state.db` keeps it.
  pub(super) fn end(&self, id: &str) {
    lock(&self.live).retain(|s| s.id != id);
  }
}

/// A failure to read `state.db`, for the caller.
fn stored(error: store::Error) -> ControlError {
  tracing::error!("state.db: {error:#}");
  ControlError::Internal(format!("cannot read the runs: {error}"))
}

pub(super) fn no_such_run() -> ControlError {
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
    _ => Err(
      ControlError::Invalid(format!("run {id} is ambiguous: {} runs match", matches.len()))
        .with_diagnostic("run.ambiguous", [("run".into(), id.into())]),
    ),
  }
}
