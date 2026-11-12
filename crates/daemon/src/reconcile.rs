//! Ownership and reconciliation: who ends a run's instances and processes,
//! and what a control plane does with what it finds after a restart or when
//! a runner connects again.
//!
//! A runner owns the processes it runs for a run: each carries the run's
//! ID. When a run ends, the control plane tells each runner it holds an
//! executor on to end the run (`EndRun`). A runner that connects reports
//! the processes of runs it still runs; the control plane claims those of
//! live runs and ends the others.
//!
//! A control plane that starts finds in `state.db` the runs it left open.
//! It does not adopt their actors (see `needy_core_actor::driver::reconcile`):
//! each such run ends `failed` (`stopped` if it was stopping), its actor is
//! ended through its driver, its control-host executor instances through
//! theirs, and its instances on
//! runners wait for the runners' reports for [`REPORT_WAIT`]; those nobody
//! reports by then are marked lost.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result};
use needy_api_client::proto::v1::ProcessInfo;
use needy_common::process::Exit;
use needy_common::time::now_ms;
use needy_config::profile::{ActorDriver, ExecutorDriver};
use needy_core::types::RunState;

use crate::store::{self, InstanceKind, InstanceRecord, InstanceState, ProcessRecord, ProcessState};
use crate::{Daemon, actor};

/// How long a restarted control plane waits for runners to report the
/// instances its open runs held on them before it marks them lost.
// NOTICE: four times the runners' 15 s reconnect backoff cap
// (`runner::RECONNECT_MAX`), so every runner that is up and reachable has
// connected and reported by then, a slow tunnel included. It is shorter than
// a runner's own grace period (`runner::DISCONNECT_GRACE`, 2 minutes): a
// runner that reports in time is told what to end; one that does not ends the
// processes of runs itself when its grace period passes.
pub const REPORT_WAIT: Duration = Duration::from_secs(60);

impl Daemon {
  /// Tells `runner` that run `run` ended, and records how its processes
  /// there ended. A runner that cannot be reached ends them when it reports
  /// again, or when its grace period passes.
  pub(crate) async fn end_on_runner(&self, run: &str, runner: &str) {
    match self.links.end_run(runner, run).await {
      Ok(ended) => self.record_ended(&ended.processes, &ended.ended).await,
      Err(status) => tracing::warn!(
        "run {run}: cannot end its processes on runner {runner}: {}; the runner ends them when it connects again, or after its grace period",
        status.message()
      ),
    }
  }

  /// Records how the listed processes of a run ended: those in `ended`
  /// were killed for it, the others that do not run exited.
  pub(crate) async fn record_ended(&self, processes: &[ProcessInfo], ended: &[String]) {
    let now = now_ms();
    for process in processes {
      let state = if ended.contains(&process.process_id) {
        ProcessState::Ended
      } else if !process.running {
        ProcessState::Exited
      } else {
        continue;
      };
      let exit = process.exited.as_ref().map(|e| Exit {
        code: e.code,
        signal: e.signal.clone(),
      });
      self.advance_process(&process.run_id, &process.process_id, state, exit, now).await;
    }
  }

  /// What a runner that connected reported: the processes of runs it
  /// still runs. Those of a live run that holds an executor on it are
  /// claimed; the runner is told to end every other run.
  pub(crate) async fn reconcile_runner(&self, runner: &str, processes: Vec<ProcessInfo>) {
    let mut by_run: BTreeMap<String, Vec<ProcessInfo>> = BTreeMap::new();
    for process in processes.into_iter().filter(|p| !p.run_id.is_empty()) {
      by_run.entry(process.run_id.clone()).or_default().push(process);
    }
    for (run_id, processes) in &by_run {
      let holder = self
        .runs
        .exact(run_id)
        .filter(|run| matches!(run.state.get(), RunState::Creating | RunState::Running))
        .and_then(|run| run.executors().into_iter().find(|(_, instance)| instance.on_runner() == Some(runner)).map(|(name, _)| name));
      match holder {
        Some(executor) => self.claim(runner, run_id, &executor, processes).await,
        None => {
          tracing::info!("runner {runner} still runs {} processes of run {run_id}, which is not live; ending them", processes.len());
          self.end_on_runner(run_id, runner).await;
        }
      }
    }
    // The runner reported everything it runs for runs: what a restart left
    // of the runs that are no longer live is gone from it now.
    let left = match self.state.instances_in(InstanceState::Running).await {
      Ok(left) => left,
      Err(e) => {
        tracing::warn!("state.db: cannot read the instances on runner {runner}: {e:#}");
        return;
      }
    };
    let now = now_ms();
    for instance in left.into_iter().filter(|i| i.runner.as_deref() == Some(runner) && self.runs.exact(&i.run).is_none()) {
      self.advance_instance(&instance.run, &instance.id, InstanceState::Ended, now).await;
      let reported: Vec<&str> = by_run.get(&instance.run).into_iter().flatten().map(|p| p.process_id.as_str()).collect();
      self.end_processes_of(&instance, ProcessState::Exited, &reported, now).await;
    }
  }

  /// Claims the processes a runner reported for a live run: they keep
  /// running, and `state.db` knows them.
  async fn claim(&self, runner: &str, run: &str, executor: &str, processes: &[ProcessInfo]) {
    tracing::info!("runner {runner}: claimed {} processes of run {run}", processes.len());
    for process in processes {
      // A process `state.db` has running is known; one it has ended cannot
      // run again, and `start_process` refuses it with the state it ended in.
      match self.state.process(&process.process_id).await {
        Ok(Some(known)) if known.state == ProcessState::Running => continue,
        Ok(_) => {}
        Err(e) => tracing::warn!("state.db: cannot read process {}: {e:#}", process.process_id),
      }
      let record = ProcessRecord {
        id: process.process_id.clone(),
        run: run.to_string(),
        instance: store::executor_instance(run, executor),
        // NOTICE: a runner does not keep who started a process, and one
        // this control plane recorded is never written again (above).
        origin: None,
        state: ProcessState::Running,
        exit: None,
        started_ms: process.started_at_ms,
        ended_ms: None,
      };
      self.start_process(&record).await;
    }
  }

  /// Reconciles the runs a previous control plane left open in
  /// `state.db`, and waits in the background for runners to report the
  /// instances they held. Called once as the daemon starts.
  pub(crate) async fn reconcile_open_runs(&self) -> Result<()> {
    let open = self.state.runs_in(&[RunState::Creating, RunState::Running, RunState::Stopping]).await.context("cannot read the runs")?;
    let mut on_runners = false;
    for mut record in open {
      let before = record.state;
      let now = now_ms();
      let instances = self.state.instances_of(&record.id).await.context("cannot read the instances")?;
      for instance in instances.iter().filter(|i| i.state == InstanceState::Running) {
        match (instance.kind, &instance.runner) {
          (InstanceKind::Actor, _) => {
            let state = self.end_leftover_actor(instance).await;
            self.advance_instance(&instance.run, &instance.id, state, now).await;
          }
          // The runner reports it when it connects; see `reconcile_runner`.
          (InstanceKind::Executor, Some(_)) => on_runners = true,
          (InstanceKind::Executor, None) => {
            let state = self.end_leftover_executor(instance).await;
            self.advance_instance(&instance.run, &instance.id, state, now).await;
            let ended = if state == InstanceState::Ended {
              ProcessState::Ended
            } else {
              ProcessState::Lost
            };
            self.end_processes_of(instance, ended, &[], now).await;
          }
        }
      }
      // The run's profile names its actor's driver, whose sockets these are.
      match needy_config::profile::load(&needy_config::Source::text(record.profile_yaml.clone(), PathBuf::new())) {
        Ok(profile) => actor::remove_sockets(self, &profile.actor, &self.run_dir(&record.id)),
        Err(e) => tracing::warn!("run {}: cannot read its profile to remove its actor's sockets: {e:#}", record.id),
      }
      record.state = if before == RunState::Stopping {
        RunState::Stopped
      } else {
        record.failure = Some(match before {
          RunState::Creating => "the control plane restarted while the run was being created; its actor was lost".into(),
          _ => "the control plane restarted; the actor was lost".into(),
        });
        RunState::Failed
      };
      record.updated_ms = now;
      // TODO(trace): the run's trace gets no `run.fail` for this: a new
      // `Dispatch` would number its events from 0 again. Appending to a
      // run's trace across a restart needs the trace to continue the run's
      // sequence; `state.db` and `run ls` carry the reason meanwhile.
      self.state.save_run(&record).await.context("cannot write a reconciled run")?;
      tracing::warn!("run {} ended {} at start: {}", record.id, record.state, record.failure.as_deref().unwrap_or("it was stopping"));
    }
    if on_runners {
      let daemon = self.clone();
      let wait = self.config.report_wait;
      self.tasks.spawn(async move {
        tokio::select! {
          () = tokio::time::sleep(wait) => daemon.mark_unreported_lost().await,
          () = daemon.cancel.cancelled() => {}
        }
      });
    }
    Ok(())
  }

  /// Marks lost the instances on runners of runs that are no longer live
  /// that no runner reported within the wait, and their processes.
  async fn mark_unreported_lost(&self) {
    let left = match self.state.instances_in(InstanceState::Running).await {
      Ok(left) => left,
      Err(e) => {
        tracing::warn!("state.db: cannot read the instances left to report: {e:#}");
        return;
      }
    };
    let now = now_ms();
    for instance in left.into_iter().filter(|i| i.runner.is_some() && self.runs.exact(&i.run).is_none()) {
      tracing::warn!(
        "run {}: runner {} did not report executor {} within {:?}; it is lost",
        instance.run,
        instance.runner.as_deref().unwrap_or_default(),
        instance.name,
        self.config.report_wait
      );
      self.advance_instance(&instance.run, &instance.id, InstanceState::Lost, now).await;
      self.end_processes_of(&instance, ProcessState::Lost, &[], now).await;
    }
  }

  /// Ends, through its driver, the actor a previous control plane started
  /// for a run, and says whether it did (`Ended`) or found none (`Lost`).
  /// See `needy_core_actor::driver::reconcile` for why it is not adopted.
  async fn end_leftover_actor(&self, instance: &InstanceRecord) -> InstanceState {
    let Some(driver) = ActorDriver::ALL.into_iter().find(|d| d.as_str() == instance.driver) else {
      tracing::warn!("run {}: its actor's driver {:?} is unknown to this daemon", instance.run, instance.driver);
      return InstanceState::Lost;
    };
    let leftover = needy_core_actor::driver::Leftover {
      run: &instance.run,
      dir: Path::new(&instance.handle),
      pid: instance.pid.and_then(|pid| u32::try_from(pid).ok()),
    };
    if needy_core_actor::driver::reconcile(driver, &leftover).await {
      InstanceState::Ended
    } else {
      InstanceState::Lost
    }
  }

  /// Ends, through its driver, an executor instance a previous control
  /// plane created for a run on the control host, and says whether it did.
  async fn end_leftover_executor(&self, instance: &InstanceRecord) -> InstanceState {
    let driver = ExecutorDriver::ALL.into_iter().find(|d| d.as_str() == instance.driver).and_then(|d| self.pool.driver(d));
    let Some(driver) = driver else {
      tracing::warn!("run {}: executor driver {:?} is not available to end {}", instance.run, instance.driver, instance.handle);
      return InstanceState::Lost;
    };
    if driver.reconcile(&instance.handle).await {
      InstanceState::Ended
    } else {
      InstanceState::Lost
    }
  }

  /// Moves run `run`'s hold on instance `id` to `to` in `state.db`. A
  /// change its lifecycle refuses is logged, as a run's is: it is a race
  /// of two enders, and the first one's state stays.
  pub(crate) async fn advance_instance(&self, run: &str, id: &str, to: InstanceState, now: i64) {
    match self.state.advance_instance(id, to, now).await {
      Ok(_) => {}
      Err(store::Error::Refused(refused)) => tracing::debug!("run {run}: {refused}"),
      Err(e) => tracing::warn!("run {run}: cannot write instance {id} to state.db: {e:#}"),
    }
  }

  /// Records in `state.db` that a process of run `run` started.
  pub(crate) async fn start_process(&self, record: &ProcessRecord) {
    match self.state.start_process(record).await {
      Ok(()) => {}
      Err(store::Error::Refused(refused)) => tracing::warn!("run {}: {refused}", record.run),
      Err(e) => tracing::warn!("run {}: cannot write process {} to state.db: {e:#}", record.run, record.id),
    }
  }

  /// Ends process `id` of run `run` in `to` in `state.db`. A change its
  /// lifecycle refuses is logged, as a run's is: a process that a relay saw
  /// exit and that the run's end lists again keeps its first end.
  pub(crate) async fn advance_process(&self, run: &str, id: &str, to: ProcessState, exit: Option<Exit>, now: i64) {
    match self.state.advance_process(id, to, exit, now).await {
      Ok(_) => {}
      Err(store::Error::Refused(refused)) => tracing::debug!("run {run}: {refused}"),
      Err(e) => tracing::warn!("run {run}: cannot write the end of process {id} to state.db: {e:#}"),
    }
  }

  /// Ends in `state` the processes `state.db` has running on `instance`,
  /// but those in `keep`, which a runner reported and was told to end.
  async fn end_processes_of(&self, instance: &InstanceRecord, state: ProcessState, keep: &[&str], now: i64) {
    let processes = match self.state.processes_of(&instance.run).await {
      Ok(processes) => processes,
      Err(e) => {
        tracing::warn!("state.db: cannot read the processes of run {}: {e:#}", instance.run);
        return;
      }
    };
    for process in
      processes.iter().filter(|p| p.instance == instance.id && p.state == ProcessState::Running && !keep.contains(&p.id.as_str()))
    {
      self.advance_process(&instance.run, &process.id, state, None, now).await;
    }
  }
}

#[cfg(test)]
#[path = "reconcile_test.rs"]
mod tests;
