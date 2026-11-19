//! Regression coverage for fixture teardown and creating-CLI child ownership.

use super::processes::{Cleanup, list};
use super::*;

const CLIENT_PROFILE: &str = r#"
actor: { driver: linux-firecracker }
executors:
  here: { driver: native, runner: { client: true } }
commands:
  - { match: "**", to: here }
"#;

fn assert_no_processes(home: &Path) {
  let deadline = Instant::now() + Duration::from_secs(3);
  loop {
    let remaining = list(home).unwrap();
    if remaining.is_empty() {
      return;
    }
    assert!(Instant::now() < deadline, "fixture left processes alive: {remaining:?}");
    std::thread::sleep(Duration::from_millis(20));
  }
}

// ROOT CAUSE:
// Before the fix, fixture teardown killed control but left admitted work alive
// and deleted its recovery state. Teardown must stop work before control exits.
#[test]
fn fixture_teardown_reclaims_started_actors_and_execution_holders() {
  for profile in [NATIVE_PROFILE, CLIENT_PROFILE] {
    let home = Home::new();
    let _cleanup = Cleanup(home.path().to_path_buf());
    let daemon = Daemon::new(&home, spawn_recording_control(&home, false));
    create_run(&home, profile);
    wait_for_fixture_pid(&home.path().join("actor-starts"));
    let running = list(home.path()).unwrap();
    assert!(running.iter().any(|p| p.kind == "runtime-holder"));
    assert!(running.iter().any(|p| p.kind == "actor"));
    if profile == CLIENT_PROFILE {
      assert!(running.iter().any(|p| p.kind == "execution-holder"));
    }

    let path = home.path().to_path_buf();
    drop(daemon);
    drop(home);

    assert!(!path.exists(), "fixture teardown must remove temporary state");
    assert_no_processes(&path);
  }
}

// ROOT CAUSE:
// Before the fix, startup created its cleanup guard after waiting for readiness.
// A timeout panic therefore dropped Child without killing or reaping control.
#[test]
fn readiness_timeout_reaps_control_before_unwinding() {
  use std::os::unix::fs::OpenOptionsExt as _;

  let home = Home::new();
  let runtime = home.path().join("run");
  needy_api_server::create_private_dir(&runtime).unwrap();
  let lock =
    std::fs::OpenOptions::new().create(true).truncate(false).write(true).mode(0o600).open(runtime.join("holder-start.lock")).unwrap();
  lock.lock().unwrap();
  // Keep the startup lock held through fallback cleanup so a failed test
  // cannot release control into a partially removed fixture.
  let _cleanup = Cleanup(home.path().to_path_buf());

  let started = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Daemon::start(&home)));

  assert!(started.is_err(), "the held startup lock must cause a readiness timeout");
  assert!(list(home.path()).unwrap().is_empty(), "startup panic left its control child alive");
}

#[test]
fn fixture_teardown_reclaims_detached_processes_outside_runs() {
  let home = Home::new();
  let _cleanup = Cleanup(home.path().to_path_buf());
  let daemon = Daemon::start(&home);
  let pid_file = home.path().join("standalone.pid");
  let _process = FixtureProcesses(vec![pid_file.clone()]);
  let script = format!("echo $$ > {}; exec sleep 300", shell_words::quote(&pid_file.display().to_string()));
  let output = home.needy().args(["proc", "exec", "--host", "-d", "--", "sh", "-c", &script]).output().unwrap();
  assert!(output.status.success(), "{}", stderr(&output));
  let pid = wait_for_fixture_pid(&pid_file);

  drop(daemon);

  assert!(gone(pid), "fixture left its detached host process alive");
  assert_no_processes(home.path());
}

// ROOT CAUSE:
// Before the fix, keep() discarded the child's wait handle before interactive
// attach. A helper exiting while its creating CLI was alive became a zombie.
#[test]
fn interactive_create_reaps_its_execution_holder_while_attached() {
  let home = Home::new();
  let _cleanup = Cleanup(home.path().to_path_buf());
  let _daemon = Daemon::new(&home, spawn_recording_control(&home, false));
  let profile = home.write("interactive.yaml", CLIENT_PROFILE);
  // No guest terminal is served, so the real CLI stays in attach while its
  // helper exits. The guard reaps that CLI even if the assertion fails.
  let mut creator = ChildGuard(Some(
    home.needy().args(["run", "create", "-it", "-f"]).arg(profile).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap(),
  ));
  let creator = creator.0.as_mut().unwrap();
  wait_for_fixture_pid(&home.path().join("actor-starts"));
  let holder = list(home.path()).unwrap().into_iter().find(|p| p.kind == "execution-holder").unwrap();
  assert_eq!(holder.parent, creator.id() as i32, "the holder must belong to the creating CLI");

  kill(holder.pid as u32, "KILL");

  let deadline = Instant::now() + Duration::from_secs(2);
  while !gone_now(holder.pid as u32) {
    assert!(creator.try_wait().unwrap().is_none(), "creating CLI exited before the reaping observation");
    assert!(Instant::now() < deadline, "creating CLI did not reap its exited helper");
    std::thread::sleep(Duration::from_millis(20));
  }
  assert!(creator.try_wait().unwrap().is_none(), "helper must be reaped while its creating CLI is still alive");
}
