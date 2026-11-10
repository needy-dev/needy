//! Runs, the instances they hold and the processes those ran, as the
//! control plane writes them at each change of state, so `run ls` and a
//! restarted control plane find them.

use needy_common::process::Exit;
use needy_core::types::{Origin, RunState};
use sea_orm::sea_query::OnConflict;
use sea_orm::{
  ActiveModelTrait as _, ActiveValue::Set, ColumnTrait as _, EntityTrait as _, IntoActiveModel as _, Iterable as _, QueryFilter as _,
  QueryOrder as _,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::entity::{instance, process, run};
use super::{Error, Result, Store};

/// A run as the control plane last wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRecord {
  pub id: String,
  pub name: String,
  pub group: String,
  pub profile_name: String,
  /// The profile the client resolved, as YAML.
  pub profile_yaml: String,
  pub state: RunState,
  pub created_ms: i64,
  pub updated_ms: i64,
  pub actor_exit: Option<Exit>,
  pub failure: Option<String>,
  /// The runner the actor was placed on; `None` for the control host.
  pub actor_runner: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceKind {
  Actor,
  Executor,
}

/// Where a run's hold on an instance stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceState {
  /// The run holds it.
  Running,
  /// The run gave it back when it ended.
  Released,
  /// Ended by reconciliation: a restarted control plane, or a runner that
  /// reported it for a run that is no longer live.
  Ended,
  /// The control plane restarted and nobody reported it: whether it still
  /// runs is unknown.
  Lost,
}

/// A run's hold on an actor or an executor instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceRecord {
  /// [`actor_instance`] or [`executor_instance`].
  pub id: String,
  pub run: String,
  /// The executor's name, or `actor`.
  pub name: String,
  pub kind: InstanceKind,
  /// The runner that owns it; `None` for the control host.
  pub runner: Option<String>,
  /// `firecracker`, `native` or `docker`.
  pub provider: String,
  /// As `run ls` shows it: `native`, `docker`, `runner`, `firecracker`.
  pub runtime: String,
  /// The container's name, the runner's name or `local`; the VM's
  /// directory for an actor.
  pub handle: String,
  /// The VM's Firecracker process.
  pub pid: Option<i64>,
  pub state: InstanceState,
  pub created_ms: i64,
  pub ended_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
  Running,
  /// It exited; its exit is unknown when nobody saw it.
  Exited,
  /// Killed because its run ended.
  Ended,
  /// The control plane restarted and nobody reported it.
  Lost,
}

/// A process one of a run's executors ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRecord {
  /// The ID its executor gave it.
  pub id: String,
  pub run: String,
  /// The instance row it runs on.
  pub instance: String,
  /// `None` for a process a runner reported that this control plane did
  /// not start.
  pub origin: Option<Origin>,
  pub state: ProcessState,
  pub exit: Option<Exit>,
  pub started_ms: i64,
  pub ended_ms: Option<i64>,
}

/// The ID of the instance row of run `run`'s actor.
pub fn actor_instance(run: &str) -> String {
  format!("{run}:actor")
}

/// The ID of the instance row of run `run`'s executor `name`.
pub fn executor_instance(run: &str, name: &str) -> String {
  format!("{run}:executor:{name}")
}

impl Store {
  /// Writes `record` whole, replacing an earlier version.
  pub async fn save_run(&self, record: &RunRecord) -> Result<()> {
    let row = run::Model {
      id: record.id.clone(),
      name: record.name.clone(),
      group_name: record.group.clone(),
      profile_name: record.profile_name.clone(),
      profile_yaml: record.profile_yaml.clone(),
      state: word(&record.state)?,
      created_ms: record.created_ms,
      updated_ms: record.updated_ms,
      actor_exit_code: record.actor_exit.as_ref().and_then(|e| e.code),
      actor_exit_signal: record.actor_exit.as_ref().map(|e| e.signal.clone()),
      failure: record.failure.clone(),
      actor_runner: record.actor_runner.clone(),
    };
    let update: Vec<run::Column> = run::Column::iter().filter(|c| !matches!(c, run::Column::Id)).collect();
    run::Entity::insert(row.into_active_model())
      .on_conflict(OnConflict::column(run::Column::Id).update_columns(update).to_owned())
      .exec(&self.db)
      .await?;
    Ok(())
  }

  pub async fn run(&self, id: &str) -> Result<Option<RunRecord>> {
    run::Entity::find_by_id(id).one(&self.db).await?.map(run_of).transpose()
  }

  /// Every run, oldest first.
  pub async fn runs(&self) -> Result<Vec<RunRecord>> {
    let rows = run::Entity::find().order_by_asc(run::Column::CreatedMs).order_by_asc(run::Column::Id).all(&self.db).await?;
    rows.into_iter().map(run_of).collect()
  }

  /// The runs in one of `states`, oldest first.
  pub async fn runs_in(&self, states: &[RunState]) -> Result<Vec<RunRecord>> {
    let states: Vec<String> = states.iter().map(word).collect::<Result<_>>()?;
    let rows = run::Entity::find()
      .filter(run::Column::State.is_in(states))
      .order_by_asc(run::Column::CreatedMs)
      .order_by_asc(run::Column::Id)
      .all(&self.db)
      .await?;
    rows.into_iter().map(run_of).collect()
  }

  /// Writes `record` whole, replacing an earlier version.
  pub async fn save_instance(&self, record: &InstanceRecord) -> Result<()> {
    let row = instance::Model {
      id: record.id.clone(),
      run: record.run.clone(),
      name: record.name.clone(),
      kind: word(&record.kind)?,
      runner: record.runner.clone(),
      provider: record.provider.clone(),
      runtime: record.runtime.clone(),
      handle: record.handle.clone(),
      pid: record.pid,
      state: word(&record.state)?,
      created_ms: record.created_ms,
      ended_ms: record.ended_ms,
    };
    let update: Vec<instance::Column> = instance::Column::iter().filter(|c| !matches!(c, instance::Column::Id)).collect();
    instance::Entity::insert(row.into_active_model())
      .on_conflict(OnConflict::column(instance::Column::Id).update_columns(update).to_owned())
      .exec(&self.db)
      .await?;
    Ok(())
  }

  /// Moves instance `id` to `state`; an instance that is no longer
  /// running records `now` as its end. An unknown ID changes nothing.
  pub async fn set_instance_state(&self, id: &str, state: InstanceState, now: i64) -> Result<()> {
    let Some(row) = instance::Entity::find_by_id(id).one(&self.db).await? else {
      return Ok(());
    };
    let mut row = row.into_active_model();
    row.state = Set(word(&state)?);
    row.ended_ms = Set((state != InstanceState::Running).then_some(now));
    row.update(&self.db).await?;
    Ok(())
  }

  /// The instances of run `run`, by ID.
  pub async fn instances_of(&self, run: &str) -> Result<Vec<InstanceRecord>> {
    let rows = instance::Entity::find().filter(instance::Column::Run.eq(run)).order_by_asc(instance::Column::Id).all(&self.db).await?;
    rows.into_iter().map(instance_of).collect()
  }

  /// The instances in `state`, by ID.
  pub async fn instances_in(&self, state: InstanceState) -> Result<Vec<InstanceRecord>> {
    let rows =
      instance::Entity::find().filter(instance::Column::State.eq(word(&state)?)).order_by_asc(instance::Column::Id).all(&self.db).await?;
    rows.into_iter().map(instance_of).collect()
  }

  /// Writes `record` whole, replacing an earlier version.
  pub async fn save_process(&self, record: &ProcessRecord) -> Result<()> {
    let row = process::Model {
      id: record.id.clone(),
      run: record.run.clone(),
      instance: record.instance.clone(),
      origin: record.origin.as_ref().map(word).transpose()?,
      state: word(&record.state)?,
      exit_code: record.exit.as_ref().and_then(|e| e.code),
      exit_signal: record.exit.as_ref().map(|e| e.signal.clone()),
      started_ms: record.started_ms,
      ended_ms: record.ended_ms,
    };
    let update: Vec<process::Column> = process::Column::iter().filter(|c| !matches!(c, process::Column::Id)).collect();
    process::Entity::insert(row.into_active_model())
      .on_conflict(OnConflict::column(process::Column::Id).update_columns(update).to_owned())
      .exec(&self.db)
      .await?;
    Ok(())
  }

  /// Ends process `id` in `state` with `exit`, if known, at `now`. An
  /// unknown ID changes nothing.
  pub async fn end_process(&self, id: &str, state: ProcessState, exit: Option<Exit>, now: i64) -> Result<()> {
    let Some(row) = process::Entity::find_by_id(id).one(&self.db).await? else {
      return Ok(());
    };
    let mut row = row.into_active_model();
    row.state = Set(word(&state)?);
    row.exit_code = Set(exit.as_ref().and_then(|e| e.code));
    row.exit_signal = Set(exit.map(|e| e.signal));
    row.ended_ms = Set(Some(now));
    row.update(&self.db).await?;
    Ok(())
  }

  pub async fn process(&self, id: &str) -> Result<Option<ProcessRecord>> {
    process::Entity::find_by_id(id).one(&self.db).await?.map(process_of).transpose()
  }

  /// The processes of run `run`, oldest first.
  pub async fn processes_of(&self, run: &str) -> Result<Vec<ProcessRecord>> {
    let rows = process::Entity::find()
      .filter(process::Column::Run.eq(run))
      .order_by_asc(process::Column::StartedMs)
      .order_by_asc(process::Column::Id)
      .all(&self.db)
      .await?;
    rows.into_iter().map(process_of).collect()
  }
}

/// A unit enum's serde name, such as `running`.
fn word(value: &impl Serialize) -> Result<String> {
  match serde_json::to_value(value)? {
    serde_json::Value::String(word) => Ok(word),
    other => Err(Error::Invalid(format!("{other} is not a single word"))),
  }
}

fn parse_word<T: DeserializeOwned>(word: &str) -> Result<T> {
  Ok(serde_json::from_value(serde_json::Value::String(word.to_string()))?)
}

fn exit(code: Option<i32>, signal: Option<String>) -> Option<Exit> {
  signal.map(|signal| Exit { code, signal })
}

fn run_of(row: run::Model) -> Result<RunRecord> {
  Ok(RunRecord {
    state: parse_word(&row.state)?,
    actor_exit: exit(row.actor_exit_code, row.actor_exit_signal),
    id: row.id,
    name: row.name,
    group: row.group_name,
    profile_name: row.profile_name,
    profile_yaml: row.profile_yaml,
    created_ms: row.created_ms,
    updated_ms: row.updated_ms,
    failure: row.failure,
    actor_runner: row.actor_runner,
  })
}

fn instance_of(row: instance::Model) -> Result<InstanceRecord> {
  Ok(InstanceRecord {
    kind: parse_word(&row.kind)?,
    state: parse_word(&row.state)?,
    id: row.id,
    run: row.run,
    name: row.name,
    runner: row.runner,
    provider: row.provider,
    runtime: row.runtime,
    handle: row.handle,
    pid: row.pid,
    created_ms: row.created_ms,
    ended_ms: row.ended_ms,
  })
}

fn process_of(row: process::Model) -> Result<ProcessRecord> {
  Ok(ProcessRecord {
    origin: row.origin.as_deref().map(parse_word).transpose()?,
    state: parse_word(&row.state)?,
    exit: exit(row.exit_code, row.exit_signal),
    id: row.id,
    run: row.run,
    instance: row.instance,
    started_ms: row.started_ms,
    ended_ms: row.ended_ms,
  })
}

#[cfg(test)]
#[path = "runs_test.rs"]
mod tests;
