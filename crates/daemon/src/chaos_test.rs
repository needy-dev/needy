use std::time::Duration;

use futures_util::StreamExt as _;
use needy_api_server::control::{ExecFault, Guest, GuestControl, GuestControls, Runs, StopMode, Target};
use needy_core::chaos::FaultSource;
use needy_core::intercept::exec::ExecContext;
use needy_core::types::Principal;

use crate::testing::{self, new_run};

const PROFILE: &str = r#"
actor: { driver: linux-firecracker }
executors:
  here: { driver: native }
commands:
  - { match: "python3 **", to: here }
chaos:
  faults:
    crash: { exec: "python3 -c **", signal: SIGSEGV, stderr: "Segmentation fault\n" }
"#;

fn ctx(argv: &[&str]) -> ExecContext {
  ExecContext {
    argv: argv.iter().map(|a| a.to_string()).collect(),
    cwd: "/".into(),
    ..ExecContext::default()
  }
}

#[tokio::test]
async fn the_proxy_is_told_how_a_fault_ends_the_command() {
  let started = testing::daemon().await;
  let created = Runs::create(&started.daemon, new_run(PROFILE, started.dir.path())).await.unwrap();

  let route = Runs::decide(&started.daemon, &created.id, ctx(&["python3", "-c", "pass"])).await.unwrap();

  assert_eq!(
    route.target,
    Target::Fault(ExecFault {
      name: "crash".into(),
      exit: None,
      signal: Some("SIGSEGV".into()),
      hang: false,
      stderr: "Segmentation fault\n".into(),
    })
  );
  let route = Runs::decide(&started.daemon, &created.id, ctx(&["python3", "-V"])).await.unwrap();
  assert_eq!(route.target, Target::Executor("here".into()));
}

#[tokio::test]
async fn an_added_fault_applies_to_the_next_command() {
  let started = testing::daemon().await;
  let created = Runs::create(&started.daemon, new_run(PROFILE, started.dir.path())).await.unwrap();

  let added = Runs::add_fault(&started.daemon, &created.id, "busy", "{ exec: \"python3 -V\", exit: 3 }", Principal::Operator).await.unwrap();
  assert!(added.warnings.is_empty(), "{:?}", added.warnings);
  let route = Runs::decide(&started.daemon, &created.id, ctx(&["python3", "-V"])).await.unwrap();
  assert!(matches!(route.target, Target::Fault(ExecFault { exit: Some(3), .. })), "{route:?}");

  let added =
    Runs::add_fault(&started.daemon, &created.id, "curl-down", "{ exec: \"curl **\", exit: 7 }", Principal::Operator).await.unwrap();
  assert!(added.warnings.is_empty(), "{:?}", added.warnings);

  let listed = Runs::faults(&started.daemon, &created.id).await.unwrap();
  let names: Vec<(&str, FaultSource, u64)> = listed.faults.iter().map(|f| (f.name.as_str(), f.source, f.fired)).collect();
  assert_eq!(
    names,
    [
      ("busy", FaultSource::Added, 1),
      ("crash", FaultSource::Profile, 0),
      ("curl-down", FaultSource::Added, 0)
    ]
  );
}

/// The next route list on `controls`, the actor's control stream.
async fn next_routes(controls: &mut GuestControls) -> Vec<String> {
  match tokio::time::timeout(Duration::from_secs(5), controls.next()).await.expect("no route list within 5 s") {
    Some(GuestControl::Routes(names)) => names,
    Some(other) => panic!("a route list, not {other:?}"),
    None => panic!("the control stream ended"),
  }
}

// ROOT CAUSE:
//
// If `needy chaos add` added an exec fault for a command no rule or profile
// fault named, the fault never fired in the actor, because the actor's init
// read its route list once at boot and the command ran in the actor without
// asking the daemon.
//
// Before the fix, the daemon only warned that the actor did not route it.
// The fix sends the actor's init the new route list on its control stream
// whenever `chaos add` or `rm` changes it.
#[tokio::test]
async fn chaos_add_and_rm_send_the_actor_its_new_route_list() {
  let started = testing::daemon().await;
  let created = Runs::create(&started.daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
  let mut controls = Guest::control(&started.daemon, &created.id).await.unwrap();
  let booted = next_routes(&mut controls).await;
  assert!(booted.contains(&"python3".to_string()) && !booted.contains(&"curl".to_string()), "{booted:?}");

  Runs::add_fault(&started.daemon, &created.id, "curl-down", "{ exec: \"curl **\", exit: 7 }", Principal::Operator).await.unwrap();
  let added = next_routes(&mut controls).await;
  Runs::remove_fault(&started.daemon, &created.id, "curl-down", Principal::Operator).await.unwrap();
  let removed = next_routes(&mut controls).await;
  Runs::add_fault(&started.daemon, &created.id, "any", "{ exec: \"** --force\", exit: 1 }", Principal::Operator).await.unwrap();
  let every = next_routes(&mut controls).await;

  let mut with_curl = booted.clone();
  with_curl.push("curl".into());
  with_curl.sort();
  assert_eq!(added, with_curl);
  assert_eq!(removed, booted);
  assert_eq!(every, ["*"], "a pattern that may match any program routes every exec");
  // A new actor's configuration has the list as it is now.
  let config = Guest::config(&started.daemon, &created.id).await.unwrap();
  assert_eq!(config.route_names, ["*"]);
}

#[tokio::test]
async fn the_control_stream_ends_with_the_run() {
  let started = testing::daemon().await;
  let created = Runs::create(&started.daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
  let mut controls = Guest::control(&started.daemon, &created.id).await.unwrap();
  next_routes(&mut controls).await;

  Runs::stop(&started.daemon, &created.id, Principal::Operator, StopMode::Now).await.unwrap();

  let ended = tokio::time::timeout(Duration::from_secs(5), controls.next()).await.expect("the stream outlived the run");
  assert_eq!(ended, None);
}

#[tokio::test]
async fn bad_additions_and_removals_are_refused() {
  let started = testing::daemon().await;
  let created = Runs::create(&started.daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
  let add = |name: &'static str, fault: &'static str| Runs::add_fault(&started.daemon, &created.id, name, fault, Principal::Operator);

  let err = add("crash", "{ exec: \"x\", exit: 1 }").await.unwrap_err().to_string();
  assert!(err.contains("already has a fault named \"crash\""), "{err}");
  let err = add("hot", "{ sensor: { temp_c: 99 } }").await.unwrap_err().to_string();
  assert!(err.contains("sensor faults are scheduled when the actor starts"), "{err}");
  let err = add("two words", "{ exec: \"x\", exit: 1 }").await.unwrap_err().to_string();
  assert!(err.contains("a fault's name is a word"), "{err}");
  let err = add("bad", "{ exec: \"x\", errno: EIO }").await.unwrap_err().to_string();
  assert!(err.contains("errno is not for exec faults"), "{err}");
  let err = Runs::remove_fault(&started.daemon, &created.id, "nope", Principal::Operator).await.unwrap_err().to_string();
  assert!(err.contains("has no fault named \"nope\""), "{err}");
}
