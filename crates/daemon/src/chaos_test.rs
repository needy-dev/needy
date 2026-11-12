use needy_api_server::control::{ExecFault, Runs, Target};
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
async fn an_added_fault_applies_to_the_next_command_and_warns_what_the_actor_does_not_route() {
  let started = testing::daemon().await;
  let created = Runs::create(&started.daemon, new_run(PROFILE, started.dir.path())).await.unwrap();

  let added = Runs::add_fault(&started.daemon, &created.id, "busy", "{ exec: \"python3 -V\", exit: 3 }", Principal::Operator).await.unwrap();
  assert!(added.warnings.is_empty(), "{:?}", added.warnings);
  let route = Runs::decide(&started.daemon, &created.id, ctx(&["python3", "-V"])).await.unwrap();
  assert!(matches!(route.target, Target::Fault(ExecFault { exit: Some(3), .. })), "{route:?}");

  // `curl` is on no rule, so the actor runs it without asking.
  let added =
    Runs::add_fault(&started.daemon, &created.id, "curl-down", "{ exec: \"curl **\", exit: 7 }", Principal::Operator).await.unwrap();
  assert_eq!(added.warnings.len(), 1);
  assert!(added.warnings[0].contains("does not reach curl"), "{:?}", added.warnings);

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
