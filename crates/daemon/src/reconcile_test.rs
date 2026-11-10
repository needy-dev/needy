//! Ownership and reconciliation through a control plane and a runner in
//! one process; the runner dials through a proxy the test can cut or take
//! down.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use needy_api_server::control::Runs;
use needy_common::process::Exit;
use needy_core::types::{RunState, RunnerRole};

use crate::store::{
  self, InstanceKind, InstanceRecord, InstanceState, ProcessRecord, ProcessState, RunRecord, actor_instance, executor_instance,
};
use crate::testing::{self, JoinedRunner, Proxy, Served, new_run};

const CONTROL_TOKEN: &str = "control-secret";

const PROFILE: &str = r#"
executors:
  far: { runner: far }
commands:
  - { match: "**", to: far }
"#;

/// A control plane and runner `far`, joined through a proxy.
struct Far {
  served: Served,
  proxy: Proxy,
  _runner: JoinedRunner,
}

async fn far(grace: Duration) -> Far {
  let served = testing::serve(testing::daemon().await, Some(CONTROL_TOKEN)).await;
  let proxy = Proxy::start(served.dir(), &served.endpoint);
  let operator = served.client.clone().with_token(CONTROL_TOKEN).unwrap();
  let config = crate::runner::Config {
    state_dir: Default::default(),
    control: Some(proxy.endpoint.clone()),
    join_token: None,
    name: "far".into(),
    roles: [RunnerRole::Executor].into(),
    labels: BTreeMap::new(),
    grace,
  };
  let runner = testing::join_runner_with(&served, &operator, config).await;
  Far {
    served,
    proxy,
    _runner: runner,
  }
}

/// Waits until `check` holds, for at most 10 s.
async fn eventually(what: &str, mut check: impl AsyncFnMut() -> bool) {
  let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
  while !check().await {
    assert!(tokio::time::Instant::now() < deadline, "{what}");
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
}

/// Cuts the runner's connection and waits until it connected again.
async fn reconnect(far: &Far) {
  let daemon = far.served.daemon();
  let before = daemon.links.get("far").unwrap();
  far.proxy.cut();
  eventually("the runner connects again", async || daemon.links.get("far").is_some_and(|now| !Arc::ptr_eq(&now, &before))).await;
}

async fn process_state(daemon: &crate::Daemon, id: &str) -> Option<ProcessState> {
  daemon.state.process(id).await.unwrap().map(|p| p.state)
}

#[tokio::test]
async fn a_runner_that_connects_again_keeps_the_processes_of_a_live_run() {
  let far = far(crate::runner::DISCONNECT_GRACE).await;
  let daemon = far.served.daemon();
  let run = Runs::create(daemon, new_run(PROFILE, far.served.dir())).await.unwrap();
  let (_, pid) = testing::detached_sleep(daemon, &run.id).await;

  reconnect(&far).await;
  // The report is reconciled as it arrives; a claim leaves the process be.
  tokio::time::sleep(Duration::from_millis(500)).await;

  assert!(testing::running(pid), "the process of a live run was ended when its runner connected again");
  assert_eq!(Runs::get(daemon, &run.id).await.unwrap().state, RunState::Running);
  Runs::stop(daemon, &run.id, needy_core::types::Principal::Operator).await.unwrap();
  assert!(testing::ended(pid).await, "process {pid} outlived its run");
}

#[tokio::test]
async fn a_runner_that_connects_again_is_told_to_end_a_run_that_stopped_meanwhile() {
  let far = far(crate::runner::DISCONNECT_GRACE).await;
  let daemon = far.served.daemon();
  let run = Runs::create(daemon, new_run(PROFILE, far.served.dir())).await.unwrap();
  let (process, pid) = testing::detached_sleep(daemon, &run.id).await;
  far.proxy.down();
  eventually("the runner disconnects", async || daemon.links.get("far").is_none()).await;
  Runs::stop(daemon, &run.id, needy_core::types::Principal::Operator).await.unwrap();
  let while_away = testing::running(pid);

  far.proxy.restore();

  assert!(while_away, "the runner ended the process while it could not be told");
  assert!(testing::ended(pid).await, "process {pid} of a stopped run outlived its runner's reconnect");
  eventually("the end is recorded", async || process_state(daemon, &process).await == Some(ProcessState::Ended)).await;
}

#[tokio::test]
async fn a_runner_that_cannot_reach_the_control_plane_ends_the_processes_of_runs_after_its_grace_period() {
  let far = far(Duration::from_millis(300)).await;
  let daemon = far.served.daemon();
  let run = Runs::create(daemon, new_run(PROFILE, far.served.dir())).await.unwrap();
  let (_, pid) = testing::detached_sleep(daemon, &run.id).await;
  let (outside_id, outside) = testing::detached_sleep_on_runner(daemon, "far").await;

  far.proxy.down();

  assert!(testing::ended(pid).await, "process {pid} outlived its runner's grace period");
  assert!(testing::running(outside), "a process outside any run belongs to no run and stays");
  far.proxy.restore();
  eventually("the runner connects again", async || daemon.links.get("far").is_some()).await;
  Runs::stop(daemon, &run.id, needy_core::types::Principal::Operator).await.unwrap();
  needy_api_server::control::Processes::kill(daemon, &outside_id, "SIGKILL", needy_core::types::Principal::Operator).await.unwrap();
}

// ---------------------------------------------------- control-plane restart

/// A stand-in for an actor's Firecracker a killed control plane left
/// running: its command line names the run's `vm.json`.
fn leftover_vm(run_dir: &std::path::Path) -> std::process::Child {
  std::fs::create_dir_all(run_dir).unwrap();
  let config = run_dir.join("vm.json");
  std::process::Command::new("sh")
    .args([
      "-c",
      "while :; do sleep 0.05; done",
      "firecracker",
      "--no-api",
      "--config-file",
    ])
    .arg(&config)
    .spawn()
    .unwrap()
}

fn instance(run: &str, id: String, name: &str, kind: InstanceKind, runner: Option<&str>, provider: &str) -> InstanceRecord {
  InstanceRecord {
    id,
    run: run.into(),
    name: name.into(),
    kind,
    runner: runner.map(str::to_string),
    provider: provider.into(),
    runtime: if runner.is_some() { "runner" } else { provider }.into(),
    handle: runner.unwrap_or("local").into(),
    pid: None,
    state: InstanceState::Running,
    created_ms: 1,
    ended_ms: None,
  }
}

/// What a control plane killed during run `r1` left in `state.db`: the run
/// running, its actor's VM (still running), a native executor on the
/// control host, and one on runner `far` with a process.
async fn left_by_a_killed_control_plane(state_dir: &std::path::Path, vm: &std::process::Child) {
  std::fs::create_dir_all(state_dir).unwrap();
  let store = store::Store::open(&state_dir.join("state.db")).await.unwrap();
  let run = RunRecord {
    id: "r1".into(),
    name: "r1".into(),
    group: "r1".into(),
    profile_name: "test".into(),
    profile_yaml: PROFILE.into(),
    state: RunState::Running,
    created_ms: 1,
    updated_ms: 1,
    actor_exit: None,
    failure: None,
    actor_runner: None,
  };
  store.save_run(&run).await.unwrap();
  let mut actor = instance("r1", actor_instance("r1"), "actor", InstanceKind::Actor, None, "firecracker");
  actor.handle = state_dir.join("runs/r1").display().to_string();
  actor.pid = Some(i64::from(vm.id()));
  store.save_instance(&actor).await.unwrap();
  store.save_instance(&instance("r1", executor_instance("r1", "here"), "here", InstanceKind::Executor, None, "native")).await.unwrap();
  store.save_instance(&instance("r1", executor_instance("r1", "far"), "far", InstanceKind::Executor, Some("far"), "native")).await.unwrap();
  let process = ProcessRecord {
    id: "p1".into(),
    run: "r1".into(),
    instance: executor_instance("r1", "far"),
    origin: Some(needy_core::types::Origin::Cli),
    state: ProcessState::Running,
    exit: None,
    started_ms: 1,
    ended_ms: None,
  };
  store.save_process(&process).await.unwrap();
}

#[tokio::test]
async fn a_restarted_control_plane_fails_a_running_run_ends_its_vm_and_marks_unreported_instances_lost() {
  let dir = tempfile::tempdir().unwrap();
  let state_dir = dir.path().join("state");
  let mut vm = leftover_vm(&state_dir.join("runs/r1"));
  left_by_a_killed_control_plane(&state_dir, &vm).await;

  let started = testing::daemon_in(dir, |config| config.report_wait = Duration::from_secs(2)).await;
  let daemon = &started.daemon;
  let failed = Runs::get(daemon, "r1").await.unwrap();
  let vm_exit = exited_within(&mut vm, Duration::from_secs(10));
  let before_the_wait = daemon.state.instances_of("r1").await.unwrap();
  eventually("the unreported instance and its process are marked lost", async || {
    let instances = daemon.state.instances_of("r1").await.unwrap();
    instances.iter().all(|i| i.state != InstanceState::Running) && process_state(daemon, "p1").await == Some(ProcessState::Lost)
  })
  .await;

  assert_eq!(failed.state, RunState::Failed);
  assert_eq!(failed.failure.as_deref(), Some("the control plane restarted; the actor was lost"));
  use std::os::unix::process::ExitStatusExt;
  assert_eq!(vm_exit.and_then(|exit| exit.signal()), Some(libc::SIGKILL), "the leftover VM was killed");
  let state = |instances: &[InstanceRecord], name: &str| instances.iter().find(|i| i.name == name).unwrap().state;
  assert_eq!(state(&before_the_wait, "actor"), InstanceState::Ended);
  assert_eq!(state(&before_the_wait, "here"), InstanceState::Lost);
  assert_eq!(state(&before_the_wait, "far"), InstanceState::Running, "waits for runner far to report");
  let after = daemon.state.instances_of("r1").await.unwrap();
  assert_eq!(state(&after, "far"), InstanceState::Lost);
  assert_eq!(process_state(daemon, "p1").await, Some(ProcessState::Lost));
  let listed = Runs::list(daemon).await.unwrap();
  let lost: Vec<&str> = listed[0].lost.iter().map(|e| e.name.as_str()).collect();
  assert_eq!(lost, ["far", "here"]);
  assert!(!state_dir_has_sockets(&started.dir.path().join("state/runs/r1")));
}

#[tokio::test]
async fn a_restarted_control_plane_leaves_a_process_that_took_a_vms_pid_alone() {
  let dir = tempfile::tempdir().unwrap();
  let state_dir = dir.path().join("state");
  // Another program runs under the PID the VM had.
  let mut other = std::process::Command::new("sleep").arg("30").spawn().unwrap();
  left_by_a_killed_control_plane(&state_dir, &other).await;

  let started = testing::daemon_in(dir, |_| {}).await;
  let actor = started.daemon.state.instances_of("r1").await.unwrap().into_iter().find(|i| i.kind == InstanceKind::Actor).unwrap();
  let still_running = other.try_wait().unwrap().is_none();
  other.kill().unwrap();
  other.wait().unwrap();

  assert!(still_running, "a process whose command line does not name the run's vm.json was killed");
  assert_eq!(actor.state, InstanceState::Lost);
}

#[tokio::test]
async fn a_run_that_was_stopping_when_the_control_plane_died_ends_stopped() {
  let dir = tempfile::tempdir().unwrap();
  let state_dir = dir.path().join("state");
  std::fs::create_dir_all(&state_dir).unwrap();
  let store = store::Store::open(&state_dir.join("state.db")).await.unwrap();
  let mut stopping = RunRecord {
    id: "r2".into(),
    name: "r2".into(),
    group: "r2".into(),
    profile_name: "test".into(),
    profile_yaml: PROFILE.into(),
    state: RunState::Stopping,
    created_ms: 1,
    updated_ms: 1,
    actor_exit: Some(Exit {
      code: Some(0),
      signal: String::new(),
    }),
    failure: None,
    actor_runner: None,
  };
  store.save_run(&stopping).await.unwrap();
  drop(store);

  let started = testing::daemon_in(dir, |_| {}).await;

  let ended = Runs::get(&started.daemon, "r2").await.unwrap();
  stopping.state = RunState::Stopped;
  assert_eq!((ended.state, ended.failure, ended.actor_exit), (RunState::Stopped, None, stopping.actor_exit));
}

fn state_dir_has_sockets(run_dir: &std::path::Path) -> bool {
  ["v.sock", "v.sock_7000", "v.sock_7001"].iter().any(|name| run_dir.join(name).exists())
}

/// How `child` exited, if it did within `bound`; killed otherwise.
fn exited_within(child: &mut std::process::Child, bound: Duration) -> Option<std::process::ExitStatus> {
  let deadline = std::time::Instant::now() + bound;
  while std::time::Instant::now() < deadline {
    if let Some(status) = child.try_wait().unwrap() {
      return Some(status);
    }
    std::thread::sleep(Duration::from_millis(20));
  }
  child.kill().unwrap();
  child.wait().unwrap();
  None
}
