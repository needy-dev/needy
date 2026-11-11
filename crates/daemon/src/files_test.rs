use needy_api_server::control::{ControlError, Runs};
use needy_core::types::{Principal, RunState};

use crate::testing::{self, new_run};

fn profile(files: &str) -> String {
  format!(
    r#"
user: {{ name: agent, uid: 1000, gid: 1000 }}
actor: {{ runtime: firecracker }}
executors:
  here: {{ provider: native }}
commands:
  - {{ match: "**", to: here }}
files:
{files}
"#
  )
}

// ROOT CAUSE:
//
// If a run's `host:` copy-in named a path that did not exist on the control
// host, such as a path of the remote client that sent the profile, packing
// it found nothing and the run went on with nothing copied.
//
// Before the fix, the run was created and finished without the files.
// The fix checks every `host:` path the run uses on the control host before
// the run acquires anything, and fails the create naming the path and where
// it was looked for.
#[tokio::test]
async fn a_copy_in_whose_host_path_is_missing_on_the_control_host_fails_the_create() {
  let started = testing::daemon().await;
  let missing = started.dir.path().join("client-only").join("seed");
  let yaml = profile(&format!(r#"  seed: {{ mode: copy-in, from: "host:{}", to: "actor:~/seed/" }}"#, missing.display()));

  let created = Runs::create(&started.daemon, new_run(&yaml, started.dir.path())).await;

  let Err(ControlError::Failed(message)) = created else {
    panic!("a copy-in of a missing host path must fail the create, not {created:?}");
  };
  assert!(message.contains("copy-in seed"), "{message}");
  assert!(message.contains(&format!("host path {} does not exist on the control host", missing.display())), "{message}");
  let listed = Runs::list(&started.daemon).await.unwrap();
  assert_eq!(listed[0].state, RunState::Failed);
}

// ROOT CAUSE:
//
// If a run's `host:` mirror-out named a directory under one the control host
// does not have, such as a remote client's home directory, the control
// daemon created the whole tree, as the user it runs as.
//
// Before the fix, `/Users/...` appeared on a Linux control host.
// The fix creates a mirror-out destination only under a directory that
// exists on the control host, and refuses the run otherwise.
#[tokio::test]
async fn a_mirror_out_under_a_directory_the_control_host_does_not_have_fails_the_create() {
  let started = testing::daemon().await;
  let elsewhere = started.dir.path().join("client-home");
  let destination = elsewhere.join("out");
  let yaml = profile(&format!(r#"  results: {{ mode: mirror-out, from: "actor:~/out/", to: "host:{}" }}"#, destination.display()));

  let created = Runs::create(&started.daemon, new_run(&yaml, started.dir.path())).await;

  let Err(ControlError::Failed(message)) = created else {
    panic!("a mirror-out the control host cannot hold must fail the create, not {created:?}");
  };
  assert!(message.contains("mirror-out results"), "{message}");
  assert!(message.contains(&format!("{} does not exist on the control host", elsewhere.display())), "{message}");
  assert!(!elsewhere.exists(), "the refused mirror-out created its tree");
}

#[tokio::test]
async fn a_mirror_out_that_fails_at_the_stop_is_recorded_for_the_report() {
  let started = testing::daemon().await;
  let source = started.dir.path().join("produced");
  std::fs::create_dir_all(&source).unwrap();
  std::fs::write(source.join("result.txt"), "done").unwrap();
  let anchor = started.dir.path().join("results");
  std::fs::create_dir_all(&anchor).unwrap();
  let yaml = profile(&format!(
    r#"  results: {{ mode: mirror-out, from: "executor:here:{}", to: "host:{}" }}"#,
    source.display(),
    anchor.join("out").display()
  ));
  let created = Runs::create(&started.daemon, new_run(&yaml, started.dir.path())).await.unwrap();
  // The directory the destination would go under is gone by the stop.
  std::fs::remove_dir_all(&anchor).unwrap();

  Runs::stop(&started.daemon, &created.id, Principal::Operator).await.unwrap();

  assert!(!anchor.exists(), "the mirror-out created a tree the control host did not have");
  let trace = needy_trace::sink::sqlite::load(&started.dir.path().join("state/trace.db"), &created.id).await.unwrap();
  let failed = trace.events.iter().find(|e| e.kind == "files.fail").expect("a files.fail event");
  assert!(failed.payload.get().contains(r#""name":"results""#), "{}", failed.payload.get());
  assert!(failed.payload.get().contains("does not exist on the control host"), "{}", failed.payload.get());
}
