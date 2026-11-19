//! Regressions for test-process cleanup and invocation stop ownership. Each probe owns its
//! temporary processes and cleans them even when the invariant fails.
//! Evidence uses real control/client/helper processes and a stand-in actor;
//! it does not establish real VM or container shutdown behavior.
//! TODO: Real runtime-holder loss and never-submitted reservation expiry need
//! separate e2e/transport fault probes; see the limits and reopening conditions
//! in docs/ai/references/cli/2026-11-19-process-lifecycle-probe-evidence.md.

use super::*;

use super::processes::{Cleanup, Process, list as processes};

/// Owns the creating CLI even when a probe assertion unwinds.
struct CreatingCli(Child);

impl Drop for CreatingCli {
  fn drop(&mut self) {
    if self.0.try_wait().unwrap().is_none() {
      self.0.kill().unwrap();
    }
    self.0.wait().unwrap();
  }
}

fn wait_for_exit(home: &Path) -> Vec<Process> {
  let deadline = Instant::now() + Duration::from_secs(3);
  loop {
    let left = processes(home);
    if left.is_empty() || Instant::now() >= deadline {
      return left;
    }
    std::thread::sleep(Duration::from_millis(20));
  }
}

fn client_profile() -> String {
  NATIVE_PROFILE.replace("here: { driver: native }", "here: { driver: native, runner: { client: true } }")
}

fn wait_for_invocation_exit(home: &Home) {
  // Client holders learn that admission ended through control/heartbeats.
  // Keep control available, as the existing client cleanup regression does.
  let deadline = Instant::now() + Duration::from_secs(12);
  loop {
    // The runtime holder also serves control's native executor and may stay
    // leased until control exits. Only invocation-owned work must end here.
    let work: Vec<_> = processes(home.path()).into_iter().filter(|p| matches!(p.kind, "actor" | "execution-holder")).collect();
    if work.is_empty() {
      return;
    }
    assert!(Instant::now() < deadline, "invocation left owned work while control was alive: {work:?}");
    std::thread::sleep(Duration::from_millis(50));
  }
}

fn stop_run(home: &Home, run: &str) {
  let stopped = home.run(&["run", "stop", "--now", run]);
  assert!(stopped.status.success(), "{}", stderr(&stopped));
}

fn finish(home: Home, daemon: Daemon) {
  let path = home.path().to_path_buf();
  drop(daemon);
  assert!(wait_for_exit(&path).is_empty(), "control exit left owned work: {:?}", processes(&path));
  drop(home);
}

fn started_actor(profile: &str) -> (Home, Daemon, Cleanup, String) {
  let home = Home::new();
  let cleanup = Cleanup(home.path().to_path_buf());
  let daemon = Daemon::new(&home, spawn_recording_control(&home, false));
  let run = create_run(&home, profile);
  wait_for_fixture_pid(&home.path().join("actor-starts"));
  let running = processes(home.path());
  assert!(running.iter().any(|p| p.kind == "runtime-holder"));
  assert!(running.iter().any(|p| p.kind == "actor"));
  (home, daemon, cleanup, run)
}

// ROOT CAUSE:
// Before the fix, the fixture killed control without stopping admitted runs. A started actor
// keeps its independent runtime holder alive after TempDir deletes recovery
// state; the stand-in actor polls that still-live holder forever.
#[test]
fn fixture_drop_ends_started_actor_and_holder() {
  assert_fixture_drop_ends_work(NATIVE_PROFILE);
}

#[test]
fn fixture_drop_ends_client_execution_holder() {
  let profile = NATIVE_PROFILE.replace("here: { driver: native }", "here: { driver: native, runner: { client: true } }");
  assert_fixture_drop_ends_work(&profile);
}

fn assert_fixture_drop_ends_work(profile: &str) {
  let (home, daemon, cleanup, _run) = started_actor(profile);
  if profile.contains("runner: { client: true }") {
    assert!(processes(home.path()).iter().any(|p| p.kind == "execution-holder"), "the client fixture must actually launch its holder");
  }
  let path = home.path().to_path_buf();
  drop(daemon);
  drop(home);
  assert!(!path.exists(), "the test's recovery directory must have been deleted");
  let left = wait_for_exit(&path);
  let orphaned = left.iter().filter(|p| p.parent == 1).count();
  let zombies = left.iter().filter(|p| p.state.starts_with('Z')).count();
  eprintln!("remaining={left:?}; orphaned={orphaned}; zombies={zombies}");
  assert!(left.is_empty(), "fixture teardown left its actor or holder alive");
  drop(cleanup);
}

#[test]
fn explicit_stop_then_fixture_drop_leaves_no_processes() {
  let profile = NATIVE_PROFILE.replace("here: { driver: native }", "here: { driver: native, runner: { client: true } }");
  let (home, daemon, cleanup, run) = started_actor(&profile);
  assert!(processes(home.path()).iter().any(|p| p.kind == "execution-holder"));
  let stopped = home.run(&["run", "stop", "--now", &run]);
  assert!(stopped.status.success(), "{}", stderr(&stopped));
  // Client holders learn that admission ended through control/heartbeats.
  // Keep control available for asynchronous cleanup, as the existing
  // client_execution_automatically_uses_creating_account_and_cleans_up test does.
  let deadline = Instant::now() + Duration::from_secs(12);
  loop {
    // The runtime holder also serves control's native executor and may stay
    // leased until control exits. Only invocation-owned work must end here.
    let work: Vec<_> = processes(home.path()).into_iter().filter(|p| matches!(p.kind, "actor" | "execution-holder")).collect();
    if work.is_empty() {
      break;
    }
    assert!(Instant::now() < deadline, "explicit stop left owned work while control was alive: {work:?}");
    std::thread::sleep(Duration::from_millis(50));
  }
  let path = home.path().to_path_buf();
  drop(daemon);
  drop(home);
  assert!(wait_for_exit(&path).is_empty(), "explicit stop must end owned work");
  drop(cleanup);
}

// ROOT CAUSE:
// Before the fix, Daemon::start constructed its guard only after readiness. A panic in
// the readiness loop drops std::process::Child without killing or waiting it.
#[test]
fn readiness_panic_does_not_leave_control_running() {
  use std::os::unix::fs::OpenOptionsExt as _;

  let home = Home::new();
  let runtime = home.path().join("run");
  needy_api_server::create_private_dir(&runtime).unwrap();
  let lock =
    std::fs::OpenOptions::new().create(true).truncate(false).write(true).mode(0o600).open(runtime.join("holder-start.lock")).unwrap();
  lock.lock().unwrap();
  // Drop cleanup before releasing the lock or deleting the fixture directory.
  let _cleanup = Cleanup(home.path().to_path_buf());
  let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Daemon::start(&home)));
  assert!(result.is_err(), "the held startup lock must trigger the readiness timeout");
  let left = processes(home.path());
  eprintln!("after readiness panic: {left:?}");
  assert!(left.is_empty(), "readiness panic leaked its control child");
}

#[test]
fn idle_control_exit_reclaims_its_runtime_holder() {
  let home = Home::new();
  let _cleanup = Cleanup(home.path().to_path_buf());
  let daemon = Daemon::start(&home);
  assert!(processes(home.path()).iter().any(|p| p.kind == "runtime-holder"));
  finish(home, daemon);
}

#[test]
fn fixture_drop_ends_a_detached_process_outside_any_run() {
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
  assert!(gone(pid), "fixture teardown left a standalone native process alive");
  assert!(wait_for_exit(home.path()).is_empty());
}

#[test]
fn repeated_stop_and_three_creation_cycles_leave_no_helpers() {
  let (home, daemon, _cleanup, first) = started_actor(&client_profile());
  for cycle in 0..3 {
    let run = if cycle == 0 {
      first.clone()
    } else {
      create_run(&home, &client_profile())
    };
    stop_run(&home, &run);
    stop_run(&home, &run);
    wait_for_invocation_exit(&home);
    assert_eq!(processes(home.path()).iter().filter(|p| p.kind == "runtime-holder").count(), 1);
  }
  finish(home, daemon);
}

#[test]
fn graceful_control_restart_keeps_original_client_holder_and_actor() {
  let (home, mut daemon, _cleanup, run) = started_actor(&client_profile());
  let before = processes(home.path());
  let execution = before.iter().find(|p| p.kind == "execution-holder").unwrap().pid;
  let actor = before.iter().find(|p| p.kind == "actor").unwrap().pid;
  kill(daemon.child.id(), "TERM");
  let deadline = Instant::now() + Duration::from_secs(2);
  loop {
    if let Some(status) = daemon.child.try_wait().unwrap() {
      assert!(status.success());
      break;
    }
    assert!(Instant::now() < deadline, "control did not exit promptly");
    std::thread::sleep(Duration::from_millis(20));
  }
  drop(daemon);
  let retained = processes(home.path());
  assert!(retained.iter().any(|p| p.pid == execution));
  assert!(retained.iter().any(|p| p.pid == actor));
  let restarted = Daemon::new(&home, spawn_recording_control(&home, false));
  let output = home.run(&["run", "exec", &run, "--", "printf", "recovered"]);
  assert!(output.status.success(), "{}", stderr(&output));
  assert_eq!(output.stdout, b"recovered");
  let recovered = processes(home.path());
  assert!(recovered.iter().any(|p| p.pid == execution));
  assert!(recovered.iter().any(|p| p.pid == actor));
  assert_eq!(std::fs::read_to_string(home.path().join("actor-starts")).unwrap().lines().count(), 1);
  stop_run(&home, &run);
  wait_for_invocation_exit(&home);
  finish(home, restarted);
}

#[test]
fn stop_then_immediate_control_exit_finishes_cleanup_after_restart() {
  let (home, mut daemon, _cleanup, run) = started_actor(&client_profile());
  stop_run(&home, &run);
  // Bypass fixture teardown to preserve the abrupt control-loss boundary.
  daemon.child.kill().unwrap();
  daemon.child.wait().unwrap();
  drop(daemon);
  // Recovery state is retained: loss of control must not turn a completed
  // stop into a new actor or an unmanageable client helper.
  let restarted = Daemon::new(&home, spawn_recording_control(&home, false));
  wait_for_invocation_exit(&home);
  stop_run(&home, &run);
  assert_eq!(std::fs::read_to_string(home.path().join("actor-starts")).unwrap().lines().count(), 1);
  finish(home, restarted);
}

#[test]
fn unexpected_actor_exit_ends_run_and_client_helper() {
  let (home, daemon, _cleanup, run) = started_actor(&client_profile());
  let actor = processes(home.path()).into_iter().find(|p| p.kind == "actor").unwrap();
  kill(actor.pid as u32, "TERM");
  wait_for_invocation_exit(&home);
  let listed = home.run(&["run", "ls", "-o", "json"]);
  assert!(listed.status.success(), "{}", stderr(&listed));
  let rows: Vec<serde_json::Value> = serde_json::from_slice(&listed.stdout).unwrap();
  let row = rows.iter().find(|r| r["id"] == run).unwrap();
  assert_eq!(row["state"], "failed", "an actor without a guest exit report must be recorded as lost");
  stop_run(&home, &run);
  finish(home, daemon);
}

#[test]
fn stopping_one_client_invocation_preserves_another() {
  let (home, daemon, _cleanup, first) = started_actor(&client_profile());
  let second = create_run(&home, &client_profile());
  let parent = |run: &str| {
    let output = home.run(&[
      "run",
      "exec",
      run,
      "--",
      "sh",
      "-c",
      "printf '%s' \"$PPID\"",
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    stdout(&output)
  };
  let first_holder = parent(&first);
  let second_holder = parent(&second);
  assert_ne!(first_holder, second_holder);
  stop_run(&home, &first);
  stop_run(&home, &first);
  assert_eq!(parent(&second), second_holder);
  stop_run(&home, &second);
  wait_for_invocation_exit(&home);
  finish(home, daemon);
}

// ROOT CAUSE:
// Interactive creation retains its CLI while waiting for the actor terminal.
// keep() discarded the execution child's wait handle before entering attach;
// a holder that exits in that interval has no waiter in its still-live parent.
#[test]
fn interactive_creating_cli_reaps_an_exited_execution_holder() {
  let home = Home::new();
  let _cleanup = Cleanup(home.path().to_path_buf());
  let _daemon = Daemon::new(&home, spawn_recording_control(&home, false));
  let profile = home.write("interactive.yaml", &client_profile());
  // The stand-in actor deliberately has no guest terminal. The real CLI
  // remains in attach, giving its own child time to exit before the CLI does.
  let mut creator =
    CreatingCli(home.needy().args(["run", "create", "-it", "-f"]).arg(profile).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());
  wait_for_fixture_pid(&home.path().join("actor-starts"));
  let holder = processes(home.path()).into_iter().find(|p| p.kind == "execution-holder").unwrap();
  assert_eq!(holder.parent, creator.0.id() as i32, "the helper must be the real creating CLI's child");
  kill(holder.pid as u32, "KILL");
  let deadline = Instant::now() + Duration::from_secs(2);
  loop {
    assert!(creator.0.try_wait().unwrap().is_none(), "the CLI must still be attached during the observation");
    let state = Command::new("ps").args(["-p", &holder.pid.to_string(), "-o", "state="]).output().unwrap();
    let state = stdout(&state).trim().to_owned();
    if state.is_empty() {
      return;
    }
    if Instant::now() >= deadline {
      assert!(!state.starts_with('Z'), "real creating CLI retained its exited execution holder as {state}");
      panic!("the signalled execution holder did not exit: {state}");
    }
    std::thread::sleep(Duration::from_millis(20));
  }
}

#[test]
fn concurrent_stops_leave_no_invocation_helpers() {
  // ROOT CAUSE:
  // Sequential stop accepts terminal history, but an overlapping request
  // attempts Stopping -> Stopping and is rejected by run::State::advance.
  // Observe cleanup before asserting responses, to distinguish a response
  // inconsistency from a process leak.
  let (home, daemon, _cleanup, initial) = started_actor(&client_profile());
  let mut failures = Vec::new();
  for cycle in 0..3 {
    let run = if cycle == 0 {
      initial.clone()
    } else {
      create_run(&home, &client_profile())
    };
    let first = home.needy().args(["run", "stop", "--now", &run]).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
    let second = home.run(&["run", "stop", "--now", &run]);
    let first = first.wait_with_output().unwrap();
    for (caller, result) in [("first", first), ("second", second)] {
      if !result.status.success() {
        failures.push(format!("cycle {cycle}, {caller}: {}", stderr(&result)));
      }
    }
    wait_for_invocation_exit(&home);
  }
  finish(home, daemon);
  assert!(failures.is_empty(), "concurrent stop response failures after successful resource cleanup: {failures:?}");
}

#[test]
fn rejected_creation_eventually_reclaims_its_client_helper() {
  let home = Home::new();
  let _cleanup = Cleanup(home.path().to_path_buf());
  let daemon = Daemon::start(&home);
  let profile = client_profile().replace("actor: { driver: linux-firecracker }", "actor: { driver: linux-firecracker, runner: missing }");
  let profile = home.write("rejected.yaml", &profile);
  let created = home.needy().args(["run", "create", "-f"]).arg(profile).output().unwrap();
  assert!(!created.status.success());
  assert!(stderr(&created).contains("runner.not_found"), "{}", stderr(&created));
  // An unbound reservation is intentionally valid for two minutes. A failed
  // create reply cannot safely imply that control never admitted the work.
  // See store/client_execution.rs::RESERVATION_MS and the client usage guide.
  let started = Instant::now();
  let deadline = started + Duration::from_secs(135);
  loop {
    let helpers: Vec<_> = processes(home.path()).into_iter().filter(|p| p.kind == "execution-holder").collect();
    if helpers.is_empty() {
      break;
    }
    assert!(Instant::now() < deadline, "rejected creation retained its helper past reservation expiry: {helpers:?}");
    std::thread::sleep(Duration::from_millis(100));
  }
  eprintln!("rejected creation helper cleanup elapsed: {:?}", started.elapsed());
  finish(home, daemon);
}
