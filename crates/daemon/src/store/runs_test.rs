use needy_common::process::Exit;
use needy_core::types::{Origin, RunState};

use super::super::{Store, executor_instance};
use super::*;

const NOW: i64 = 1_000_000;

async fn store() -> (Store, tempfile::TempDir) {
  let dir = tempfile::tempdir().unwrap();
  let store = Store::open(&dir.path().join("state.db")).await.unwrap();
  (store, dir)
}

fn record(id: &str, state: RunState) -> RunRecord {
  RunRecord {
    id: id.into(),
    name: "build".into(),
    group: id.into(),
    profile_name: "opencode".into(),
    profile_yaml: "executors: {}\n".into(),
    state,
    created_ms: NOW,
    updated_ms: NOW,
    actor_exit: None,
    failure: None,
    actor_runner: Some("kvm".into()),
  }
}

fn executor(run: &str, name: &str, runner: Option<&str>) -> InstanceRecord {
  InstanceRecord {
    id: executor_instance(run, name),
    run: run.into(),
    name: name.into(),
    kind: InstanceKind::Executor,
    runner: runner.map(str::to_string),
    driver: "runner".into(),
    handle: runner.unwrap_or("local").into(),
    pid: None,
    state: InstanceState::Running,
    created_ms: NOW,
    ended_ms: None,
  }
}

#[tokio::test]
async fn a_run_keeps_every_state_it_is_written_with_across_a_reopen() {
  let (store, dir) = store().await;

  store.save_run(&record("r1", RunState::Creating)).await.unwrap();
  store.save_run(&record("r1", RunState::Running)).await.unwrap();
  let mut failed = record("r1", RunState::Failed);
  failed.updated_ms = NOW + 5;
  failed.failure = Some("the control plane restarted; the actor was lost".into());
  failed.actor_exit = Some(Exit {
    code: None,
    signal: "SIGKILL".into(),
  });
  store.save_run(&failed).await.unwrap();
  drop(store);
  let reopened = Store::open(&dir.path().join("state.db")).await.unwrap();

  assert_eq!(reopened.runs().await.unwrap(), [failed.clone()]);
  assert_eq!(reopened.run("r1").await.unwrap(), Some(failed));
  assert_eq!(reopened.run("r2").await.unwrap(), None);
}

#[tokio::test]
async fn runs_are_found_by_state() {
  let (store, _dir) = store().await;
  store.save_run(&record("r1", RunState::Running)).await.unwrap();
  store.save_run(&record("r2", RunState::Stopped)).await.unwrap();
  store.save_run(&record("r3", RunState::Creating)).await.unwrap();

  let open = store.runs_in(&[RunState::Creating, RunState::Running]).await.unwrap();

  let ids: Vec<&str> = open.iter().map(|r| r.id.as_str()).collect();
  assert_eq!(ids, ["r1", "r3"]);
}

#[tokio::test]
async fn an_instance_moves_from_running_to_lost_with_its_end() {
  let (store, _dir) = store().await;
  store.start_instance(&executor("r1", "mac", Some("mac"))).await.unwrap();
  store.start_instance(&executor("r1", "here", None)).await.unwrap();
  store.start_instance(&executor("r2", "mac", Some("mac"))).await.unwrap();

  let from = store.advance_instance(&executor_instance("r1", "mac"), InstanceState::Lost, NOW + 9).await.unwrap();

  assert_eq!(from, Some(InstanceState::Running));

  let of_r1 = store.instances_of("r1").await.unwrap();
  assert_eq!(of_r1.len(), 2);
  let mac = of_r1.iter().find(|i| i.name == "mac").unwrap();
  assert_eq!((mac.state, mac.ended_ms), (InstanceState::Lost, Some(NOW + 9)));
  let running: Vec<String> = store.instances_in(InstanceState::Running).await.unwrap().into_iter().map(|i| i.id).collect();
  assert_eq!(
    running,
    [
      executor_instance("r1", "here"),
      executor_instance("r2", "mac")
    ]
  );
}

#[tokio::test]
async fn a_process_is_recorded_running_then_ended_with_its_exit() {
  let (store, _dir) = store().await;
  let started = ProcessRecord {
    id: "p1".into(),
    run: "r1".into(),
    instance: executor_instance("r1", "mac"),
    origin: Some(Origin::ActorMount),
    state: ProcessState::Running,
    exit: None,
    started_ms: NOW,
    ended_ms: None,
  };
  store.start_process(&started).await.unwrap();
  let reported = ProcessRecord {
    id: "p2".into(),
    origin: None,
    ..started.clone()
  };
  store.start_process(&reported).await.unwrap();

  let killed = Exit {
    code: None,
    signal: "SIGKILL".into(),
  };
  let from = store.advance_process("p1", ProcessState::Ended, Some(killed.clone()), NOW + 3).await.unwrap();

  assert_eq!(from, Some(ProcessState::Running));

  let p1 = store.process("p1").await.unwrap().unwrap();
  assert_eq!((p1.state, p1.exit, p1.ended_ms), (ProcessState::Ended, Some(killed), Some(NOW + 3)));
  assert_eq!(p1.origin, Some(Origin::ActorMount));
  let all = store.processes_of("r1").await.unwrap();
  assert_eq!(all.len(), 2);
  assert_eq!(all.iter().find(|p| p.id == "p2").unwrap().origin, None, "a reported process keeps no origin");
}

#[tokio::test]
async fn an_ended_instance_keeps_its_first_end() {
  let (store, _dir) = store().await;
  let id = executor_instance("r1", "mac");
  store.start_instance(&executor("r1", "mac", Some("mac"))).await.unwrap();
  store.advance_instance(&id, InstanceState::Ended, NOW + 1).await.unwrap();

  let error = store.advance_instance(&id, InstanceState::Lost, NOW + 2).await.unwrap_err();

  assert_eq!(error.to_string(), "instance r1:executor:mac is ended; it cannot become lost");
  let mac = &store.instances_of("r1").await.unwrap()[0];
  assert_eq!((mac.state, mac.ended_ms), (InstanceState::Ended, Some(NOW + 1)));
}

#[tokio::test]
async fn an_instance_is_started_once() {
  let (store, _dir) = store().await;
  let id = executor_instance("r1", "mac");
  store.start_instance(&executor("r1", "mac", Some("mac"))).await.unwrap();
  store.advance_instance(&id, InstanceState::Released, NOW + 1).await.unwrap();

  let error = store.start_instance(&executor("r1", "mac", Some("mac"))).await.unwrap_err();

  assert!(matches!(error, Error::Refused(_)), "{error}");
  assert_eq!(error.to_string(), "instance r1:executor:mac is released; it cannot become running");
  assert_eq!(store.instances_of("r1").await.unwrap()[0].state, InstanceState::Released);
  let ended = InstanceRecord {
    state: InstanceState::Lost,
    ..executor("r2", "mac", None)
  };
  assert!(matches!(store.start_instance(&ended).await, Err(Error::Invalid(_))), "a hold starts running");
}

#[tokio::test]
async fn an_unknown_instance_or_process_changes_nothing() {
  let (store, _dir) = store().await;

  assert_eq!(store.advance_instance("r1:actor", InstanceState::Released, NOW).await.unwrap(), None);
  assert_eq!(store.advance_process("p1", ProcessState::Exited, None, NOW).await.unwrap(), None);
  assert!(store.instances_of("r1").await.unwrap().is_empty());
}

// ROOT CAUSE:
//
// If a relay recorded a process's exit and the run's end then listed the
// process again, or killed it first and the relay saw the exit after, the
// last writer's state stood, because the store wrote any state over any
// other.
//
// The fix keeps the first end: an ended process is refused another.
#[tokio::test]
async fn an_exited_process_keeps_its_exit() {
  let (store, _dir) = store().await;
  let started = ProcessRecord {
    id: "p1".into(),
    run: "r1".into(),
    instance: executor_instance("r1", "mac"),
    origin: None,
    state: ProcessState::Running,
    exit: None,
    started_ms: NOW,
    ended_ms: None,
  };
  store.start_process(&started).await.unwrap();
  let exited = Exit {
    code: Some(3),
    signal: String::new(),
  };
  store.advance_process("p1", ProcessState::Exited, Some(exited.clone()), NOW + 1).await.unwrap();

  let error = store.advance_process("p1", ProcessState::Ended, None, NOW + 2).await.unwrap_err();

  assert_eq!(error.to_string(), "process p1 is exited; it cannot become ended");
  let p1 = store.process("p1").await.unwrap().unwrap();
  assert_eq!((p1.state, p1.exit, p1.ended_ms), (ProcessState::Exited, Some(exited), Some(NOW + 1)));
  assert!(matches!(store.start_process(&started).await, Err(Error::Refused(_))), "a process is started once");
}
