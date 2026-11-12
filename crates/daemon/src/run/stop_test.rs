use std::path::Path;
use std::time::Duration;

use futures_util::StreamExt as _;
use needy_api_server::control::{Guest, GuestControl, Runs, StopMode};
use needy_core::event::ActorStop;
use needy_core::types::{Principal, RunState};

use crate::testing::{self, fake_image, new_run};

/// A run whose actor mirrors `~/out` out to `host`.
fn profile(host: &Path) -> String {
  format!(
    r#"
actor: {{ driver: linux-firecracker }}
user: {{ name: agent, uid: 1000, gid: 1000 }}
files:
  out: {{ mode: mirror-out, from: "actor:~/out/", to: "host:{}" }}
"#,
    host.display()
  )
}

/// Waits until the guest of run `id` waits for a stop, as init does once
/// its driving program runs.
async fn guest_waits(daemon: &crate::Daemon, id: &str) {
  let run = daemon.runs.find(id).unwrap();
  tokio::time::timeout(Duration::from_secs(10), async {
    while !run.stop_request.waited_for() {
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  })
  .await
  .expect("the guest did not wait for a stop");
}

/// What init does with its control stream (`Guest::control`) about a stop:
/// waits for one and returns its grace period; `Err` when the stream ends
/// without one.
async fn wait_stop(daemon: &crate::Daemon, id: &str) -> Result<Duration, ()> {
  let mut controls = Guest::control(daemon, id).await.map_err(drop)?;
  while let Some(control) = controls.next().await {
    if let GuestControl::Stop { grace } = control {
      return Ok(grace);
    }
  }
  Err(())
}

/// The `run.stop` payload of run `id`.
async fn stop_event(dir: &Path, id: &str) -> String {
  let trace = needy_trace::sink::sqlite::load(&dir.join("state/trace.db"), id).await.unwrap();
  trace.events.iter().find(|e| e.kind == "run.stop").expect("a run.stop event").payload.get().to_string()
}

// ROOT CAUSE:
//
// If a run whose driving program still ran was stopped with `needy run
// stop`, the files its actor mirrors out, such as an agent's login, never
// reached the host, because the stop cancelled the run first, which killed
// the actor, and only init mirrors the actor's side out, after the driving
// program exits.
//
// Before the fix, the stop killed the actor at once.
// The fix asks the guest that waits for it (`GuestControl::Stop` on its control stream) to end the
// driving program, and waits for the actor to mirror out and end before
// the stop goes on.
#[tokio::test]
async fn a_graceful_stop_waits_for_the_actors_mirror_out() {
  let started = testing::daemon().await;
  let daemon = started.daemon.clone();
  fake_image(&started.dir.path().join("image"), testing::REBOOTS_AFTER_EXIT);
  let host = started.dir.path().join("out");
  let created = Runs::create(&daemon, new_run(&profile(&host), started.dir.path())).await.unwrap();
  // The guest: it waits for a stop, then, as its driving program exits,
  // takes a while to mirror out, reports the exit and ends the VM.
  let source = started.dir.path().join("in-actor");
  std::fs::create_dir_all(&source).unwrap();
  std::fs::write(source.join("auth.json"), "token").unwrap();
  let guest = {
    let (daemon, id, dir) = (daemon.clone(), created.id.clone(), started.dir.path().to_path_buf());
    tokio::spawn(async move {
      let grace = wait_stop(&daemon, &id).await.unwrap();
      tokio::time::sleep(Duration::from_millis(300)).await;
      let archive = needy_common::archive::pack(&source, &[]).unwrap();
      Guest::push_files(&daemon, &id, "out", archive).await.unwrap();
      testing::exit_actor(&daemon, &dir, &id).await;
      grace
    })
  };
  guest_waits(&daemon, &created.id).await;

  let grace = Duration::from_secs(4);
  let stopped = Runs::stop(&daemon, &created.id, Principal::Operator, StopMode::Graceful { grace }).await.unwrap();

  assert_eq!(stopped, Some(ActorStop::Graceful));
  assert_eq!(guest.await.unwrap(), grace, "the guest got the stop's grace period");
  assert_eq!(std::fs::read_to_string(host.join("auth.json")).unwrap(), "token", "the actor's mirror-out reached the host");
  assert_eq!(Runs::get(&daemon, &created.id).await.unwrap().state, RunState::Stopped);
  let stop = stop_event(started.dir.path(), &created.id).await;
  assert!(stop.contains(r#""actor":{"how":"graceful"}"#), "{stop}");
}

#[tokio::test]
async fn a_stop_now_kills_the_actor_without_its_mirror_out() {
  let started = testing::daemon().await;
  let daemon = started.daemon.clone();
  fake_image(&started.dir.path().join("image"), testing::REBOOTS_AFTER_EXIT);
  let host = started.dir.path().join("out");
  let created = Runs::create(&daemon, new_run(&profile(&host), started.dir.path())).await.unwrap();
  let guest = {
    let (daemon, id) = (daemon.clone(), created.id.clone());
    tokio::spawn(async move { wait_stop(&daemon, &id).await })
  };
  guest_waits(&daemon, &created.id).await;

  let stopped = tokio::time::timeout(Duration::from_secs(10), Runs::stop(&daemon, &created.id, Principal::Operator, StopMode::Now))
    .await
    .expect("a stop now must not wait for the actor")
    .unwrap();

  assert!(matches!(stopped, Some(ActorStop::Forced { .. })), "{stopped:?}");
  assert!(guest.await.unwrap().is_err(), "the guest was not asked to stop gracefully");
  assert!(!host.exists());
  let stop = stop_event(started.dir.path(), &created.id).await;
  assert!(stop.contains(r#""how":"forced""#), "{stop}");
}

// A guest that predates graceful stops never waits for one; the stop kills
// its actor at once and says why, instead of waiting out a grace period.
#[tokio::test]
async fn a_stop_kills_an_actor_whose_guest_does_not_wait_and_says_why() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let host = started.dir.path().join("out");
  let created = Runs::create(daemon, new_run(&profile(&host), started.dir.path())).await.unwrap();

  let stopped = tokio::time::timeout(Duration::from_secs(10), Runs::stop(daemon, &created.id, Principal::Operator, StopMode::default()))
    .await
    .expect("a stop of an actor whose guest does not wait must not hang")
    .unwrap();

  let Some(ActorStop::Forced { reason }) = stopped else {
    panic!("the actor must be killed, not {stopped:?}");
  };
  assert!(reason.contains("not waiting for a stop"), "{reason}");
}

#[tokio::test]
async fn a_graceful_stop_kills_an_actor_that_does_not_end_in_time() {
  let started = testing::daemon_with(|config| config.mirror_out_wait = Duration::from_millis(200)).await;
  let daemon = started.daemon.clone();
  let host = started.dir.path().join("out");
  let created = Runs::create(&daemon, new_run(&profile(&host), started.dir.path())).await.unwrap();
  // A guest that is asked and never ends its actor.
  let guest = {
    let (daemon, id) = (daemon.clone(), created.id.clone());
    tokio::spawn(async move { wait_stop(&daemon, &id).await })
  };
  guest_waits(&daemon, &created.id).await;

  let stopped = tokio::time::timeout(
    Duration::from_secs(10),
    Runs::stop(
      &daemon,
      &created.id,
      Principal::Operator,
      StopMode::Graceful {
        grace: Duration::from_millis(100),
      },
    ),
  )
  .await
  .expect("the stop must not wait past its bound")
  .unwrap();

  assert!(guest.await.unwrap().is_ok(), "the guest was asked to stop");
  let Some(ActorStop::Forced { reason }) = stopped else {
    panic!("the actor must be killed, not {stopped:?}");
  };
  assert!(reason.contains("did not end within 0.3 s"), "{reason}");
  assert_eq!(Runs::get(&daemon, &created.id).await.unwrap().state, RunState::Stopped);
}
