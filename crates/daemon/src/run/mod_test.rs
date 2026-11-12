use std::collections::BTreeMap;

use needy_api_server::control::{ControlError, Runs};
use needy_core::event::Event;
use needy_core::intercept::ExecContext;
use needy_core::types::RunState;

use crate::testing::{self, fake_image, new_run};

const PROFILE: &str = r#"
actor: { driver: linux-firecracker }
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  here: { driver: native }
commands:
  - { match: "**", to: here }
"#;

#[tokio::test]
async fn a_created_run_runs_on_its_executor_until_stopped() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let created = Runs::create(daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
  assert_eq!(created.state, RunState::Running);
  assert_eq!(created.name, created.id, "an unnamed run is named after its ID");
  assert_eq!(created.profile, "test");
  let executors: Vec<_> = created.executors.iter().map(|e| (e.name.as_str(), e.driver.as_str())).collect();
  assert_eq!(executors, [("here", "native")]);
  assert!(started.dir.path().join("state/runs").join(&created.id).join("profile.yaml").is_file());

  Runs::stop(daemon, &created.id, needy_core::types::Principal::Operator).await.unwrap();
  let stopped = Runs::get(daemon, &created.id).await.unwrap();
  assert_eq!(stopped.state, RunState::Stopped);
  let listed = Runs::list(daemon).await.unwrap();
  assert_eq!(listed.len(), 1);
  assert_eq!(listed[0].state, RunState::Stopped);
  assert!(
    matches!(Runs::stop(daemon, &created.id, needy_core::types::Principal::Operator).await, Err(ControlError::NotFound(_))),
    "a stopped run is not stopped again"
  );
}

// ROOT CAUSE:
//
// If `needy report` ran right after `run create` returned, it sometimes
// found no run or no `run.create` (the CLI test
// `report_names_the_profile_after_its_file` failed about once in eight full
// runs), because a create only queued `run.create` on the trace's worker
// and returned without waiting for the sinks to write it.
//
// Before the fix, a create returned with its `run.create` still queued.
// The fix keeps the create's events written when the create returns, as a
// stop and a failed create already flushed theirs.
//
// The test runtime has one thread, so the trace worker runs only when the
// test awaits; reading the unbuffered JSONL sink without awaiting sees what
// the create itself waited for.
#[tokio::test]
async fn a_created_run_has_its_create_in_the_trace_when_create_returns() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let created = Runs::create(daemon, new_run(PROFILE, started.dir.path())).await.unwrap();

  let trace = started.dir.path().join("state/runs").join(&created.id).join("trace.jsonl");
  let lines = std::fs::read_to_string(&trace).unwrap_or_default();
  let kinds: Vec<String> =
    lines.lines().map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()["kind"].as_str().unwrap().to_string()).collect();
  assert!(kinds.iter().any(|kind| kind == "run.create"), "{kinds:?}");
}

#[tokio::test]
async fn a_run_whose_profile_is_invalid_is_listed_as_failed() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let profile = "actor: { driver: linux-firecracker }\ncommands:\n  - { match: \"**\", to: nowhere }\n";
  let Err(ControlError::Failed(message)) = Runs::create(daemon, new_run(profile, started.dir.path())).await else {
    panic!("an invalid profile must fail the create");
  };
  assert!(message.contains("invalid profile"), "{message}");
  let listed = Runs::list(daemon).await.unwrap();
  assert_eq!(listed.len(), 1);
  assert_eq!(listed[0].state, RunState::Failed);
}

#[tokio::test]
async fn runs_are_found_by_id_unique_prefix_or_name() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let mut request = new_run(PROFILE, started.dir.path());
  request.name = "named".into();
  let created = Runs::create(daemon, request).await.unwrap();
  assert_eq!(Runs::get(daemon, &created.id).await.unwrap().id, created.id);
  assert_eq!(Runs::get(daemon, &created.id[..4]).await.unwrap().id, created.id);
  assert_eq!(Runs::get(daemon, "named").await.unwrap().id, created.id);
  assert!(matches!(Runs::get(daemon, "zzzz").await, Err(ControlError::NotFound(_))));
}

// ROOT CAUSE:
//
// If a lookup was given an empty run ID, every run matched it as a
// prefix, so with one run `get`, `stop` and `Decide` acted on that
// run.
//
// Before the fix, `needy run stop ""` stopped the only run.
// The fix keeps an empty ID naming no run.
#[tokio::test]
async fn an_empty_id_names_no_run() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let created = Runs::create(daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
  assert!(matches!(Runs::get(daemon, "").await, Err(ControlError::NotFound(_))));
  assert!(matches!(Runs::stop(daemon, "", needy_core::types::Principal::Operator).await, Err(ControlError::NotFound(_))));
  let ctx = ExecContext {
    argv: vec!["true".into()],
    ..ExecContext::default()
  };
  assert!(matches!(Runs::decide(daemon, "", ctx).await, Err(ControlError::NotFound(_))));
  assert_eq!(Runs::get(daemon, &created.id).await.unwrap().state, RunState::Running);
}

#[tokio::test]
async fn a_prefix_of_several_runs_is_ambiguous() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  // Seventeen IDs of hex digits share a first digit at least twice.
  let mut by_first: BTreeMap<char, Vec<String>> = BTreeMap::new();
  for _ in 0..17 {
    let created = Runs::create(daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
    by_first.entry(created.id.chars().next().unwrap()).or_default().push(created.id);
  }
  let (first, ids) = by_first.iter().find(|(_, ids)| ids.len() > 1).unwrap();
  let Err(ControlError::Invalid(message)) = Runs::stop(daemon, &first.to_string(), needy_core::types::Principal::Operator).await else {
    panic!("an ambiguous prefix must be refused");
  };
  assert!(message.contains("ambiguous"), "{message}");
  for id in ids {
    assert_eq!(Runs::get(daemon, id).await.unwrap().state, RunState::Running);
  }
}

// ROOT CAUSE:
//
// If a run's create failed after it had acquired some of its executors,
// those instances stayed in the pool, because the rollback stopped the
// run's tasks and recorded the failure but released nothing.
//
// Before the fix, a profile whose second executor could not be created left
// the first one's instance pooled (for Docker, a running container).
// The fix keeps a failed create holding nothing: the rollback releases
// every instance the create acquired.
#[tokio::test]
async fn a_failed_create_releases_the_executors_it_acquired() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  // `b` is acquired after `a` and fails: the operator allows no privileged
  // container, which the `linux-container` driver refuses before it reaches
  // the engine.
  let profile = r#"
actor: { driver: linux-firecracker }
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  a: { driver: native }
  b: { driver: linux-container, container: { privileged: true } }
commands:
  - { match: "**", to: a }
"#;
  let Err(ControlError::Failed(message)) = Runs::create(daemon, new_run(profile, started.dir.path())).await else {
    panic!("a create whose executor fails must fail");
  };
  assert!(message.contains("executor b"), "{message}");

  assert!(daemon.pool.instances().await.is_empty(), "the failed create still holds an executor");
  let listed = Runs::list(daemon).await.unwrap();
  assert_eq!(listed.len(), 1);
  assert_eq!(listed[0].state, RunState::Failed);
  let released: Vec<&str> = listed[0].released.iter().map(|e| e.name.as_str()).collect();
  assert_eq!(released, ["a"], "a failed run lists the executors it gave back");
  assert!(listed[0].failure.as_deref().is_some_and(|f| f.contains("executor b")), "{:?}", listed[0].failure);
}

#[tokio::test]
async fn a_create_whose_actor_cannot_start_releases_its_executors_and_sockets() {
  use std::os::unix::fs::PermissionsExt;
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let image = started.dir.path().join("image");
  fake_image(&image, "exit 0");
  // Firecracker cannot be started once the guest listener and the gateway
  // listen.
  std::fs::set_permissions(image.join("firecracker"), std::fs::Permissions::from_mode(0o644)).unwrap();

  let Err(ControlError::Failed(message)) = Runs::create(daemon, new_run(PROFILE, started.dir.path())).await else {
    panic!("a create whose actor cannot start must fail");
  };
  assert!(message.contains("firecracker"), "{message}");

  assert!(daemon.pool.instances().await.is_empty(), "the failed create still holds an executor");
  let id = &Runs::list(daemon).await.unwrap()[0].id;
  let dir = started.dir.path().join("state/runs").join(id);
  let sockets: Vec<_> =
    std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).filter(|n| n.to_string_lossy().starts_with("v.sock")).collect();
  assert!(sockets.is_empty(), "{sockets:?}");
}

// The owner's 2026-11-10 trial: a finished or stopped run gives its
// executors back, and `run ls` then showed none at all.
#[tokio::test]
async fn a_stopped_run_lists_the_executors_it_released() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let created = Runs::create(daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
  assert!(created.released.is_empty());

  Runs::stop(daemon, &created.id, needy_core::types::Principal::Operator).await.unwrap();

  let stopped = Runs::get(daemon, &created.id).await.unwrap();
  assert!(stopped.executors.is_empty(), "{:?}", stopped.executors);
  let released: Vec<_> = stopped.released.iter().map(|e| (e.name.as_str(), e.driver.as_str())).collect();
  assert_eq!(released, [("here", "native")]);
}

#[tokio::test]
async fn a_finished_run_keeps_its_driving_programs_exit() {
  use needy_api_server::control::Guest;
  use needy_common::process::Exit;
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let created = Runs::create(daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
  assert_eq!(created.actor_exit, None);

  let exit = Exit {
    code: Some(3),
    signal: String::new(),
  };
  Guest::exit(daemon, &created.id, exit.clone()).await.unwrap();

  let finished = Runs::get(daemon, &created.id).await.unwrap();
  assert_eq!(finished.state, RunState::Finished);
  assert_eq!(finished.actor_exit, Some(exit));
  assert_eq!(finished.failure, None);
}

/// A profile whose every command runs on executor `far`, a native
/// executor on runner `far`.
const RUNNER_PROFILE: &str = r#"
actor: { driver: linux-firecracker }
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  far: { runner: far }
commands:
  - { match: "**", to: far }
"#;

const CONTROL_TOKEN: &str = "control-secret";

/// A served control plane with runner `far` joined and connected.
async fn with_runner_far() -> (testing::Served, testing::JoinedRunner) {
  let served = testing::serve(testing::daemon().await, Some(CONTROL_TOKEN)).await;
  let operator = served.client.clone().with_token(CONTROL_TOKEN).unwrap();
  let runner = testing::join_runner(&served, &operator, "far", "executor(native)", &[]).await;
  (served, runner)
}

async fn detached_sleep(daemon: &crate::Daemon, run: &str) -> i64 {
  testing::detached_sleep(daemon, run).await.1
}

async fn ended(pid: i64) -> bool {
  testing::ended(pid).await
}

// ROOT CAUSE:
//
// If a run with a remote executor stopped, its detached processes and
// its workspace mount's `sftp-server` kept running on the remote machine,
// because the `remote` provider's `destroy` does nothing and nothing else
// ended them.
//
// Before the fix, the sleep below outlived its run.
// The fix kills the processes a run started on each of its executors
// before it releases them. Runners replaced remote executors; a runner's
// machine outlives its runs in the same way.
#[tokio::test]
async fn stopping_a_run_ends_its_processes_on_a_runner() {
  let (served, _far) = with_runner_far().await;
  let daemon = served.daemon();
  let created = Runs::create(daemon, new_run(RUNNER_PROFILE, served.dir())).await.unwrap();
  let pid = detached_sleep(daemon, &created.id).await;

  Runs::stop(daemon, &created.id, needy_core::types::Principal::Operator).await.unwrap();

  assert!(ended(pid).await, "process {pid} outlived its run");
}

#[tokio::test]
async fn a_finished_run_ends_its_processes_on_a_runner() {
  let (served, _far) = with_runner_far().await;
  let daemon = served.daemon();
  fake_image(&served.dir().join("image"), testing::REBOOTS_AFTER_EXIT);
  let created = Runs::create(daemon, new_run(RUNNER_PROFILE, served.dir())).await.unwrap();
  let pid = detached_sleep(daemon, &created.id).await;
  testing::exit_actor(daemon, served.dir(), &created.id).await;

  assert!(ended(pid).await, "process {pid} outlived its finished run");
  assert_eq!(Runs::get(daemon, &created.id).await.unwrap().state, RunState::Finished);
  Runs::stop(daemon, &created.id, needy_core::types::Principal::Operator).await.unwrap();
}

// ROOT CAUSE:
//
// If the control daemon ran on another machine than the client, a profile
// with a relative `extends` or `actor.recipe` failed with `cannot read
// recipe /Users/...`, because the client sent its profile directory and the
// daemon read the files there on its own machine.
//
// Before the fix, the daemon read the client's directory.
// The fix sends the profile and its recipe resolved, as text; the client's
// directory is gone here before the run is created.
#[tokio::test]
async fn a_run_is_created_from_a_profile_whose_directory_the_daemon_cannot_read() {
  let client = tempfile::tempdir().unwrap();
  std::fs::write(client.path().join("base.yaml"), "user: { name: agent, uid: 1000, gid: 1000 }\nexecutors:\n  here: { driver: native }\n")
    .unwrap();
  std::fs::write(client.path().join("r.recipe.yaml"), "extends: builtin:gce-ubuntu-22.04\nos: { hostname: from-the-client }\n").unwrap();
  let yaml = "extends: base.yaml\nactor: { driver: linux-firecracker, recipe: r.recipe.yaml }\ncommands:\n  - { match: \"**\", to: here }\n";
  let request = new_run(yaml, client.path());
  drop(client);
  let started = testing::daemon().await;

  let created = Runs::create(&started.daemon, request).await.unwrap();

  let run = started.daemon.runs.find(&created.id).unwrap();
  assert_eq!(run.recipe.as_ref().and_then(|r| r.hostname.as_deref()), Some("from-the-client"));
  assert_eq!(created.executors.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["here"]);
}

// The GCE built-in's hostname is `instance-` and the run's creation time
// (owner decision D4, 2026-11-12); `run.create` records the time, so a
// replay compiles the same machine from it and the seed of the run's ID.
#[tokio::test]
async fn a_runs_recipe_time_is_its_creation_time_and_run_create_records_it() {
  let dir = tempfile::tempdir().unwrap();
  let yaml = "actor: { driver: linux-firecracker, recipe: \"builtin:gce-ubuntu-22.04\" }\nuser: { name: agent, uid: 1000, gid: 1000 }\nexecutors:\n  here: { driver: native }\n";
  let started = testing::daemon().await;

  let created = Runs::create(&started.daemon, new_run(yaml, dir.path())).await.unwrap();

  let time = needy_core::recipe::PerRun::at(0, created.created_ms).time;
  let run = started.daemon.runs.find(&created.id).unwrap();
  assert_eq!(run.recipe.as_ref().and_then(|r| r.hostname.clone()), Some(format!("instance-{time}")));
  let events = Runs::events(&started.daemon, &created.id).await.unwrap().events;
  let create = events.iter().find(|e| e.kind == "run.create").unwrap();
  let Event::RunCreate(payload) = Event::decode(&create.kind, create.version, &create.payload).unwrap() else {
    unreachable!()
  };
  assert_eq!(payload.recipe_time, Some(time));
}

#[tokio::test]
async fn a_create_whose_selector_names_no_connected_runner_fails_before_it_acquires_anything() {
  let (served, _far) = with_runner_far().await;
  let daemon = served.daemon();
  let profile = r#"
actor: { driver: linux-firecracker }
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  a: { driver: native }
  mac: { runner: { labels: [os=darwin] } }
"#;

  let Err(ControlError::Failed(message)) = Runs::create(daemon, new_run(profile, served.dir())).await else {
    panic!("a create whose selector names no runner must fail");
  };

  assert!(
    message.contains("executor mac: no connected runner with executor(native) has labels os=darwin; candidates: far (no label os=darwin)"),
    "{message}"
  );
  assert!(daemon.pool.instances().await.is_empty());
  let listed = Runs::list(daemon).await.unwrap();
  assert!(listed[0].released.is_empty(), "nothing was acquired: {:?}", listed[0].released);
}

#[tokio::test]
async fn an_actor_on_a_runner_other_than_the_control_hosts_own_fails_the_create() {
  let served = testing::serve(testing::daemon().await, Some(CONTROL_TOKEN)).await;
  let operator = served.client.clone().with_token(CONTROL_TOKEN).unwrap();
  let _far = testing::join_runner(&served, &operator, "far", "actor(linux-firecracker)", &[]).await;
  let profile = "user: { name: agent, uid: 1000, gid: 1000 }\nactor: { driver: linux-firecracker, runner: far }\n";

  let Err(ControlError::Failed(message)) = Runs::create(served.daemon(), new_run(profile, served.dir())).await else {
    panic!("an actor on another runner must fail the create");
  };

  assert!(message.starts_with("actor: runner far cannot host actors yet"), "{message}");
}

#[tokio::test]
async fn an_actor_on_the_control_hosts_own_runner_starts_here() {
  let started = testing::daemon_with(|config| config.local_runner = Some("kvm".into())).await;
  let served = testing::serve(started, Some(CONTROL_TOKEN)).await;
  let operator = served.client.clone().with_token(CONTROL_TOKEN).unwrap();
  let _kvm =
    testing::join_runner(&served, &operator, "kvm", "actor(linux-firecracker) executor(linux-container,native)", &[("kvm", "")]).await;
  let profile = r#"
user: { name: agent, uid: 1000, gid: 1000 }
actor: { driver: linux-firecracker, runner: { labels: [kvm] } }
executors:
  build: { runner: kvm }
commands:
  - { match: "**", to: build }
"#;

  let created = Runs::create(served.daemon(), new_run(profile, served.dir())).await.unwrap();

  assert_eq!(created.state, RunState::Running);
  let executors: Vec<_> = created.executors.iter().map(|e| (e.name.as_str(), e.driver.as_str(), e.instance.as_str())).collect();
  assert_eq!(executors, [("build", "runner", "kvm")]);
  Runs::stop(served.daemon(), &created.id, needy_core::types::Principal::Operator).await.unwrap();
}

#[tokio::test]
async fn a_runs_life_is_written_to_state_db_and_listed_after_a_restart() {
  use crate::store::{InstanceState, ProcessState, executor_instance};
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let created = Runs::create(daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
  let (process, _) = testing::detached_sleep(daemon, &created.id).await;
  let running = daemon.state.run(&created.id).await.unwrap().unwrap();
  let held = daemon.state.instances_of(&created.id).await.unwrap();

  Runs::stop(daemon, &created.id, needy_core::types::Principal::Operator).await.unwrap();
  let restarted = testing::restart(started).await;
  let daemon = &restarted.daemon;

  assert_eq!(running.state, RunState::Running);
  assert_eq!(running.profile_name, "test");
  assert!(running.profile_yaml.contains("here"), "{}", running.profile_yaml);
  let kinds: Vec<(&str, InstanceState)> = held.iter().map(|i| (i.id.as_str(), i.state)).collect();
  let actor = format!("{}:actor", created.id);
  let here = executor_instance(&created.id, "here");
  assert_eq!(
    kinds,
    [
      (actor.as_str(), InstanceState::Running),
      (here.as_str(), InstanceState::Running)
    ]
  );
  let listed = Runs::list(daemon).await.unwrap();
  assert_eq!(listed.len(), 1);
  assert_eq!((listed[0].id.as_str(), listed[0].state), (created.id.as_str(), RunState::Stopped));
  let released: Vec<&str> = listed[0].released.iter().map(|e| e.name.as_str()).collect();
  assert_eq!(released, ["here"]);
  assert_eq!(Runs::get(daemon, &created.name).await.unwrap().state, RunState::Stopped, "found by name after the restart");
  let stored = daemon.state.instances_of(&created.id).await.unwrap();
  assert!(stored.iter().all(|i| i.state == InstanceState::Released), "{stored:?}");
  let sleep = daemon.state.process(&process).await.unwrap().unwrap();
  assert_eq!((sleep.state, sleep.instance.as_str()), (ProcessState::Ended, here.as_str()));
  assert_eq!(sleep.origin, Some(needy_core::types::Origin::Cli));
}
