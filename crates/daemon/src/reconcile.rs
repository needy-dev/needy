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
//! It does not adopt their actors (see [`end_leftover_vm`]): each such run
//! ends `failed` (`stopped` if it was stopping), its actor's Firecracker or
//! container is ended, its control-host containers are removed, and its instances on
//! runners wait for the runners' reports for [`REPORT_WAIT`]; those nobody
//! reports by then are marked lost.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result};
use needy_api_client::proto::v1::ProcessInfo;
use needy_common::process::Exit;
use needy_common::time::now_ms;
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

// NOTICE: how long the control plane waits for a leftover Firecracker to
// exit after `SIGKILL`; it is reaped by its new parent at once.
const VM_END_WAIT: Duration = Duration::from_secs(5);

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
      if let Err(e) = self.state.end_process(&process.process_id, state, exit, now).await {
        tracing::warn!("run {}: cannot write the end of process {} to state.db: {e:#}", process.run_id, process.process_id);
      }
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
      self.write_instance_state(&instance, InstanceState::Ended, now).await;
      let reported: Vec<&str> = by_run.get(&instance.run).into_iter().flatten().map(|p| p.process_id.as_str()).collect();
      self.end_processes_of(&instance, ProcessState::Exited, &reported, now).await;
    }
  }

  /// Claims the processes a runner reported for a live run: they keep
  /// running, and `state.db` knows them.
  async fn claim(&self, runner: &str, run: &str, executor: &str, processes: &[ProcessInfo]) {
    tracing::info!("runner {runner}: claimed {} processes of run {run}", processes.len());
    for process in processes {
      let known = match self.state.process(&process.process_id).await {
        Ok(Some(known)) if known.state == ProcessState::Running => continue,
        Ok(known) => known,
        Err(e) => {
          tracing::warn!("state.db: cannot read process {}: {e:#}", process.process_id);
          None
        }
      };
      let record = ProcessRecord {
        id: process.process_id.clone(),
        run: run.to_string(),
        instance: store::executor_instance(run, executor),
        // NOTICE: a runner does not keep who started a process; a process
        // this control plane recorded keeps its origin, running (above) or
        // not.
        origin: known.and_then(|known| known.origin),
        state: ProcessState::Running,
        exit: None,
        started_ms: process.started_at_ms,
        ended_ms: None,
      };
      if let Err(e) = self.state.save_process(&record).await {
        tracing::warn!("run {run}: cannot write process {} to state.db: {e:#}", process.process_id);
      }
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
          (InstanceKind::Actor, _) if instance.runtime == "container" => {
            let state = end_leftover_container(instance).await;
            self.write_instance_state(instance, state, now).await;
          }
          (InstanceKind::Actor, _) => {
            // A VM's process names a file in the run's directory on its
            // command line: Firecracker its `vm.json`, vfkit its pidfile.
            let marker = match instance.runtime.as_str() {
              "vz" => needy_core_actor::host::vz::pidfile(Path::new(&instance.handle)),
              _ => Path::new(&instance.handle).join("vm.json"),
            };
            let state = end_leftover_vm(instance, &marker).await;
            self.write_instance_state(instance, state, now).await;
          }
          // The runner reports it when it connects; see `reconcile_runner`.
          (InstanceKind::Executor, Some(_)) => on_runners = true,
          (InstanceKind::Executor, None) if instance.provider == "docker" => {
            let state = self.remove_leftover_container(instance).await;
            self.write_instance_state(instance, state, now).await;
            let ended = if state == InstanceState::Ended {
              ProcessState::Ended
            } else {
              ProcessState::Lost
            };
            self.end_processes_of(instance, ended, &[], now).await;
          }
          (InstanceKind::Executor, None) => {
            // TODO(runner-ownership): the processes the control daemon's own
            // executor ran for a run die with a daemon that stops, but one
            // killed with SIGKILL leaves them orphaned and unknown to its
            // successor, which does not record their host PIDs. Native
            // executors on the control host go through its local runner
            // (`--local-runner`) to be owned like any runner's.
            self.write_instance_state(instance, InstanceState::Lost, now).await;
            self.end_processes_of(instance, ProcessState::Lost, &[], now).await;
          }
        }
      }
      // The run's profile names its actor's runtime, whose sockets these are.
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
      self.write_instance_state(&instance, InstanceState::Lost, now).await;
      self.end_processes_of(&instance, ProcessState::Lost, &[], now).await;
    }
  }

  /// Removes a leftover container of a run on the control host.
  async fn remove_leftover_container(&self, instance: &InstanceRecord) -> InstanceState {
    let Some(docker) = self.pool.docker() else {
      tracing::warn!("run {}: Docker is not available to remove container {}", instance.run, instance.handle);
      return InstanceState::Lost;
    };
    match docker.remove_instance(&instance.handle).await {
      Ok(()) => InstanceState::Ended,
      Err(e) => {
        tracing::warn!("run {}: cannot remove container {}: {e}", instance.run, instance.handle);
        InstanceState::Lost
      }
    }
  }

  async fn write_instance_state(&self, instance: &InstanceRecord, state: InstanceState, now: i64) {
    if let Err(e) = self.state.set_instance_state(&instance.id, state, now).await {
      tracing::warn!("run {}: cannot write instance {} to state.db: {e:#}", instance.run, instance.id);
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
      if let Err(e) = self.state.end_process(&process.id, state, None, now).await {
        tracing::warn!("run {}: cannot write the end of process {} to state.db: {e:#}", instance.run, process.id);
      }
    }
  }
}

/// Ends the VM process (Firecracker, vfkit) of an actor a previous control
/// plane started, if it still runs, and says whether it did (`Ended`) or
/// found none (`Lost`).
///
/// A restarted control plane does not adopt it. The VM is no longer its
/// child, so its exit cannot be collected; the guest's connections to the
/// old daemon (routed commands in flight, the gateway's) broke; and the run's
/// chaos counters and trace sequence lived in the old daemon's memory, so an
/// adopted run would no longer replay deterministically. Ending it and
/// failing the run says what happened.
// NOTICE: the PID is checked against the process's command line, which
// names `marker`, a file in the run's directory (Firecracker's `vm.json`,
// vfkit's pidfile), so a PID another process took since is left alone.
async fn end_leftover_vm(instance: &InstanceRecord, marker: &Path) -> InstanceState {
  use sysinfo::{ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, Signal, System, UpdateKind};
  let Some(pid) = instance.pid.and_then(|pid| u32::try_from(pid).ok()) else {
    return InstanceState::Lost;
  };
  let pid = sysinfo::Pid::from_u32(pid);
  let Ok(mut system) = System::new() else {
    tracing::warn!("run {}: cannot read this machine's processes to find its actor's VM", instance.run);
    return InstanceState::Lost;
  };
  let alive = |system: &mut System| {
    system.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always));
    system.process(pid).is_some_and(|p| p.status() != ProcessStatus::Zombie && p.cmd().iter().any(|arg| Path::new(arg) == marker))
  };
  if !alive(&mut system) {
    return InstanceState::Lost;
  }
  tracing::warn!("run {}: ending its actor's VM ({} {pid}), which a restarted control plane does not adopt", instance.run, instance.runtime);
  if !system.process(pid).and_then(|p| p.kill_with(Signal::Kill)).unwrap_or(false) {
    tracing::warn!("run {}: cannot kill the VM process {pid}", instance.run);
  }
  let deadline = tokio::time::Instant::now() + VM_END_WAIT;
  while alive(&mut system) {
    if tokio::time::Instant::now() >= deadline {
      tracing::warn!("run {}: the VM process {pid} did not exit within {VM_END_WAIT:?} of SIGKILL", instance.run);
      break;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
  }
  InstanceState::Ended
}

/// Ends the container of an actor a previous control plane started, and
/// detaches its host-side workspace mounts, for the reasons
/// [`end_leftover_vm`] gives. The container is found by its run's name.
async fn end_leftover_container(instance: &InstanceRecord) -> InstanceState {
  // The OCI runtime and image do not matter for ending a container.
  let runtime = needy_core_actor::host::container::Container::new(
    needy_config::profile::OciRuntime::Runc,
    needy_config::profile::CONTAINER_IMAGE.into(),
  );
  tracing::warn!("run {}: ending its actor's container, which a restarted control plane does not adopt", instance.run);
  if runtime.end_leftover(&instance.run, Path::new(&instance.handle)).await {
    InstanceState::Ended
  } else {
    InstanceState::Lost
  }
}

#[cfg(test)]
#[path = "reconcile_test.rs"]
mod tests;
