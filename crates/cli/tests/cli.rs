//! Observable behavior of the `needy` command: exit codes, messages and
//! output, against a control daemon started in a temporary directory.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// A temporary home for one test: the state, data and runtime directories,
/// and the environment that points `needy` at them.
struct Home {
  dir: tempfile::TempDir,
}

impl Home {
  fn new() -> Self {
    // NOTICE: socket paths hold at most 104 bytes on macOS, so keep the
    // directory short instead of nesting it in the target directory.
    let dir = tempfile::Builder::new().prefix("needy-").tempdir_in(std::env::temp_dir()).unwrap();
    Self { dir }
  }

  fn path(&self) -> &Path {
    self.dir.path()
  }

  /// `needy` with only this home's directories in its environment.
  fn needy(&self) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_needy"));
    for name in [
      "NEEDY_ENDPOINT",
      "NEEDY_TOKEN",
      "XDG_STATE_HOME",
      "XDG_DATA_HOME",
      "XDG_RUNTIME_DIR",
    ] {
      command.env_remove(name);
    }
    command
      .env("NEEDY_STATE_DIR", self.path().join("state"))
      .env("NEEDY_DATA_DIR", self.path().join("data"))
      .env("NEEDY_RUNTIME_DIR", self.path().join("run"))
      .stdin(Stdio::null());
    command
  }

  fn run(&self, args: &[&str]) -> Output {
    self.needy().args(args).output().unwrap()
  }

  fn write(&self, name: &str, text: &str) -> PathBuf {
    let path = self.path().join(name);
    std::fs::write(&path, text).unwrap();
    path
  }
}

/// The arguments that give a control daemon a stand-in actor: an image
/// whose `firecracker` prints two console lines and runs until the daemon
/// stops it or exits, and a stand-in KVM device, so runs start on hosts
/// without KVM. The guest never boots: commands reach a run's executors
/// through `needy exec --run`.
fn stand_in_actor(home: &Home) -> Vec<String> {
  use std::os::unix::fs::PermissionsExt;
  let image = home.path().join("image");
  std::fs::create_dir_all(&image).unwrap();
  std::fs::write(image.join("vmlinux"), "").unwrap();
  std::fs::write(image.join("rootfs.ext4"), "").unwrap();
  let firecracker = image.join("firecracker");
  let script = "#!/bin/sh\necho boot\necho hello from the actor\nwhile kill -0 $PPID 2>/dev/null; do sleep 0.05; done\n";
  std::fs::write(&firecracker, script).unwrap();
  std::fs::set_permissions(&firecracker, std::fs::Permissions::from_mode(0o755)).unwrap();
  let kvm = home.path().join("kvm");
  std::fs::write(&kvm, "").unwrap();
  vec![
    "--actor-image".into(),
    image.display().to_string(),
    "--kvm".into(),
    kvm.display().to_string(),
  ]
}

/// A control daemon on the home's default socket, stopped on drop.
struct Daemon {
  child: Child,
}

impl Daemon {
  fn start(home: &Home) -> Self {
    let child = home
      .needy()
      .args(["daemon", "--mode", "control"])
      .args(stand_in_actor(home))
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .spawn()
      .unwrap();
    let socket = home.path().join("run").join("daemon.sock");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !socket.exists() {
      assert!(Instant::now() < deadline, "the daemon did not create {}", socket.display());
      std::thread::sleep(Duration::from_millis(50));
    }
    Self { child }
  }
}

impl Drop for Daemon {
  fn drop(&mut self) {
    // The test is over; a daemon that already exited needs no kill.
    if self.child.kill().is_ok() {
      self.child.wait().unwrap();
    }
  }
}

fn stdout(output: &Output) -> String {
  String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
  String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Runs every command on this machine, refuses `rm` and fails `curl`.
const NATIVE_PROFILE: &str = r#"
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  here: { provider: native }
commands:
  - { match: "rm **", deny: no deleting here }
  - { match: "**", to: here }
chaos:
  faults:
    - { on: exec, match: "curl **", errno: ECONNREFUSED }
    - { on: exec, match: "sleep *", delay: 1ms }
"#;

/// A profile the daemon refuses when it creates the run: its default
/// executor does not exist.
const BROKEN_PROFILE: &str = "user: { name: agent, uid: 1000, gid: 1000 }\ndefault_executor: missing-executor\n";

fn create_run(home: &Home, profile: &str) -> String {
  let profile = home.write("profile.yaml", profile);
  let output = home.run(&["run", "create", "--profile", profile.to_str().unwrap()]);
  assert!(output.status.success(), "run create: {}", stderr(&output));
  stdout(&output).trim().to_string()
}

// The QA pass on 2026-11-09 found that an injected errno on the CLI path
// exited 1 with "needy: needy chaos: ECONNREFUSED"; the actor path exits 126.
#[test]
fn exec_fault_exits_126_and_says_fault_injected() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let run = create_run(&home, NATIVE_PROFILE);

  let output = home.run(&["exec", "--run", &run, "--", "curl", "http://example.com"]);

  assert_eq!(output.status.code(), Some(126));
  assert_eq!(stderr(&output), "needy: fault injected: ECONNREFUSED\n");
}

#[test]
fn exec_denied_by_a_rule_exits_126() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let run = create_run(&home, NATIVE_PROFILE);

  let output = home.run(&["exec", "--run", &run, "--", "rm", "-rf", "/"]);

  assert_eq!(output.status.code(), Some(126));
  assert_eq!(stderr(&output), "needy: denied: no deleting here\n");
}

#[test]
fn exec_fault_in_json_output_reports_the_exit_code() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let run = create_run(&home, NATIVE_PROFILE);

  let output = home.run(&[
    "exec",
    "--output",
    "json",
    "--run",
    &run,
    "--",
    "curl",
    "http://example.com",
  ]);

  assert_eq!(output.status.code(), Some(126));
  let value: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
  assert_eq!(value["exit_code"], 126);
  assert_eq!(value["error"], "fault injected: ECONNREFUSED");
}

/// The lines of `ndjson` output, parsed.
fn ndjson(output: &Output) -> Vec<serde_json::Value> {
  stdout(output).lines().map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}"))).collect()
}

fn types(lines: &[serde_json::Value]) -> Vec<&str> {
  lines.iter().map(|l| l["type"].as_str().unwrap()).collect()
}

// The QA pass on 2026-11-09 found that `--output ndjson` printed neither the
// placement decision nor the faults chaos injected.
#[test]
fn exec_ndjson_includes_the_placement_decision_and_injected_faults() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let run = create_run(&home, NATIVE_PROFILE);

  let delayed = home.run(&[
    "exec", "--output", "ndjson", "--run", &run, "--", "sleep", "0",
  ]);

  assert_eq!(delayed.status.code(), Some(0), "{}", stderr(&delayed));
  let lines = ndjson(&delayed);
  assert_eq!(types(&lines), ["fault.injected", "exec.decide", "started", "exit"]);
  assert_eq!(lines[0]["kind"], "exec");
  assert_eq!(lines[0]["target"], "sleep 0");
  assert_eq!(lines[0]["effect"]["delay"], 1);
  assert_eq!(lines[1]["argv"], serde_json::json!(["sleep", "0"]));
  assert_eq!(lines[1]["outcome"], serde_json::json!({"run": {"executor": "here"}}));
  assert_eq!(lines[1]["origin"], "cli");

  let refused = home.run(&[
    "exec",
    "--output",
    "ndjson",
    "--run",
    &run,
    "--",
    "curl",
    "http://example.com",
  ]);

  assert_eq!(refused.status.code(), Some(126));
  let lines = ndjson(&refused);
  assert_eq!(types(&lines), ["fault.injected", "exec.decide", "error", "exit"]);
  assert_eq!(lines[0]["effect"]["errno"], "ECONNREFUSED");
  assert_eq!(lines[1]["outcome"], serde_json::json!({"fault": "ECONNREFUSED"}));
  assert_eq!(lines[3]["code"], 126);
}

#[test]
fn exec_ndjson_without_a_run_has_no_decision() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);

  let output = home.run(&["exec", "--output", "ndjson", "--", "true"]);

  assert_eq!(types(&ndjson(&output)), ["started", "exit"]);
}

// ROOT CAUSE:
//
// If one run was running, an exec without `--run` was traced as that
// run's, and could meet its chaos faults, because the relay looked the
// run up by an empty ID prefix, which matches every run.
//
// Before the fix, the report of the running run listed the command.
// The fix keeps an exec without a run out of every run.
#[test]
fn exec_without_a_run_stays_out_of_a_running_run() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let run = create_run(&home, NATIVE_PROFILE);

  let output = home.run(&["exec", "--", "sh", "-c", "echo outside-a-run"]);
  assert_eq!(stdout(&output), "outside-a-run\n");
  assert!(home.run(&["run", "stop", &run]).status.success());

  let report = stdout(&home.run(&["report", &run, "--level", "detailed", "-o", "markdown"]));
  assert!(report.contains("| run.stop |"), "{report}");
  assert!(!report.contains("outside-a-run"), "{report}");
}

#[test]
fn exec_of_a_missing_command_exits_127() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);

  let output = home.run(&["exec", "--", "/nonexistent/needy-test-command"]);

  assert_eq!(output.status.code(), Some(127));
  let message = stderr(&output);
  assert!(message.starts_with("needy: cannot start /nonexistent/needy-test-command"), "{message}");
  assert_eq!(message.matches("needy:").count(), 1, "{message}");
}

#[test]
fn exec_returns_the_command_exit_code_and_output() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);

  let output = home.run(&["exec", "--", "sh", "-c", "echo out; echo err >&2; exit 7"]);

  assert_eq!(output.status.code(), Some(7));
  assert_eq!(stdout(&output), "out\n");
  assert_eq!(stderr(&output), "err\n");
}

#[test]
fn run_create_names_a_missing_profile() {
  let home = Home::new();
  let missing = home.path().join("missing.yaml");

  let output = home.run(&["run", "create", "--profile", missing.to_str().unwrap()]);

  assert_eq!(output.status.code(), Some(1));
  assert!(stderr(&output).contains(missing.to_str().unwrap()), "{}", stderr(&output));
}

#[test]
fn run_create_refuses_a_profile_with_errors() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let profile = home.write("profile.yaml", "executors: { here: { provider: native } }\ncommands: [{ match: \"**\", to: hier }]\n");

  let output = home.run(&["run", "create", "--profile", profile.to_str().unwrap()]);

  assert_eq!(output.status.code(), Some(1));
  let message = stderr(&output);
  assert!(message.contains("invalid profile: commands[0] (**) sends to unknown executor \"hier\""), "{message}");
}

#[test]
fn run_ls_shows_stopped_and_failed_runs() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let stopped = create_run(&home, NATIVE_PROFILE);
  assert!(home.run(&["run", "stop", &stopped]).status.success());
  let broken = home.write("broken.yaml", BROKEN_PROFILE);
  let failed = home.run(&[
    "run",
    "create",
    "--name",
    "broken",
    "--profile",
    broken.to_str().unwrap(),
  ]);
  assert_eq!(failed.status.code(), Some(1));

  let output = home.run(&["run", "ls", "--output", "json"]);

  assert!(output.status.success(), "{}", stderr(&output));
  let runs: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
  let state =
    |key: &str, value: &str| runs.as_array().unwrap().iter().find(|s| s[key] == value).map(|s| s["state"].as_str().unwrap().to_string());
  assert_eq!(state("id", &stopped).as_deref(), Some("stopped"));
  assert_eq!(state("name", "broken").as_deref(), Some("failed"));
}

// The QA pass on 2026-11-09 found that a failed create left nothing in the
// trace, so `needy report` could not say why the run failed.
#[test]
fn a_failed_run_create_records_run_fail_with_the_reason() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let broken = home.write("broken.yaml", BROKEN_PROFILE);
  let failed = home.run(&["run", "create", "--profile", broken.to_str().unwrap()]);
  assert_eq!(failed.status.code(), Some(1));

  let output = home.run(&["report", "latest", "-o", "markdown"]);

  assert!(output.status.success(), "{}", stderr(&output));
  let report = stdout(&output);
  let overview = report.lines().skip_while(|l| !l.starts_with("| Duration")).nth(2).unwrap();
  assert!(overview.contains("| failed |"), "{report}");
  let finding = report.lines().find(|l| l.starts_with("- ❌ Run failed: ")).unwrap_or_else(|| panic!("{report}"));
  assert!(finding.contains("missing-executor"), "{finding}");
}

// The QA pass on 2026-11-09 found that the report named a profile after its
// directory, because the run request carried no file name.
#[test]
fn report_names_the_profile_after_its_file() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let profile = home.write("review.profile.yaml", NATIVE_PROFILE);
  let created = home.run(&["run", "create", "--profile", profile.to_str().unwrap()]);
  assert!(created.status.success(), "{}", stderr(&created));
  let run = stdout(&created).trim().to_string();

  let output = home.run(&["report", &run, "-o", "markdown"]);

  assert!(output.status.success(), "{}", stderr(&output));
  let title = stdout(&output).lines().next().unwrap().to_string();
  assert_eq!(title, format!("# Run report: {run} (profile: review)"));
}

#[test]
fn run_stop_is_traced_without_attributes() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let run = create_run(&home, NATIVE_PROFILE);
  assert!(home.run(&["run", "stop", &run]).status.success());

  let output = home.run(&["report", &run, "--level", "detailed", "-o", "markdown"]);

  assert!(output.status.success(), "{}", stderr(&output));
  let report = stdout(&output);
  let stop = report.lines().find(|l| l.contains("| run.stop |")).unwrap_or_else(|| panic!("{report}"));
  assert!(stop.ends_with("| run.stop | `{}` |"), "{stop}");
  assert!(report.lines().nth(2).is_some_and(|l| !l.contains("incomplete")), "{report}");
}

#[test]
fn run_ls_prints_a_table() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let run = create_run(&home, NATIVE_PROFILE);

  let output = home.run(&["run", "ls"]);

  let text = stdout(&output);
  let mut lines = text.lines();
  let header: Vec<&str> = lines.next().unwrap().split_whitespace().collect();
  assert_eq!(header, ["ID", "NAME", "STATE", "GROUP", "EXECUTORS", "EXIT"]);
  // A name and group that default to the ID show as `-`; a running
  // run has no exit yet.
  let row: Vec<&str> = lines.next().unwrap().split_whitespace().collect();
  assert_eq!(row, [run.as_str(), "-", "running", "-", "here=native"]);
}

// The owner's 2026-11-10 trial: a run that ended gave its executors
// back and `run ls` showed an empty EXECUTORS cell, and nothing said
// how the run ended.
#[test]
fn run_ls_shows_released_executors_and_why_a_run_failed() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let stopped = create_run(&home, NATIVE_PROFILE);
  assert!(home.run(&["run", "stop", &stopped]).status.success());
  let broken = home.write("broken.yaml", BROKEN_PROFILE);
  let failed = home.run(&[
    "run",
    "create",
    "--name",
    "broken",
    "--profile",
    broken.to_str().unwrap(),
  ]);
  assert_eq!(failed.status.code(), Some(1));

  let output = home.run(&["run", "ls"]);

  let rows = table(&stdout(&output));
  assert_eq!(
    rows[1][..],
    [
      stopped.as_str(),
      "-",
      "stopped",
      "-",
      "here=native (released)"
    ]
  );
  assert_eq!(rows[2][1..3], ["broken", "failed"]);
  let exit = rows[2].last().unwrap();
  assert!(exit.starts_with("invalid profile: ") && exit.ends_with('…') && exit.chars().count() <= 49, "{:?}", rows[2]);
}

#[test]
fn run_ls_json_keeps_names_and_carries_released_executors_and_the_failure() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let stopped = create_run(&home, NATIVE_PROFILE);
  assert!(home.run(&["run", "stop", &stopped]).status.success());
  let broken = home.write("broken.yaml", BROKEN_PROFILE);
  home.run(&["run", "create", "--profile", broken.to_str().unwrap()]);

  let output = home.run(&["run", "ls", "-o", "json"]);

  let runs: Vec<serde_json::Value> = serde_json::from_str(&stdout(&output)).unwrap();
  assert_eq!((&runs[0]["name"], &runs[0]["group"]), (&serde_json::json!(stopped), &serde_json::json!(stopped)));
  assert_eq!(runs[0]["executors"], serde_json::json!([{ "name": "here", "runtime": "native", "released": true }]));
  assert_eq!(
    (&runs[0]["exit_code"], &runs[0]["signal"], &runs[0]["failure"]),
    (&serde_json::Value::Null, &serde_json::Value::Null, &serde_json::Value::Null)
  );
  assert!(runs[1]["failure"].as_str().unwrap().contains("missing-executor"), "{:?}", runs[1]);
}

#[test]
fn run_logs_prints_the_actor_console() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let run = create_run(&home, NATIVE_PROFILE);
  // The stand-in Firecracker prints two lines to the console.
  let deadline = Instant::now() + Duration::from_secs(10);
  let output = loop {
    let output = home.run(&["run", "logs", &run]);
    if stdout(&output).lines().count() == 2 || Instant::now() > deadline {
      break output;
    }
    std::thread::sleep(Duration::from_millis(50));
  };

  assert!(output.status.success(), "{}", stderr(&output));
  assert_eq!(stdout(&output), "boot\nhello from the actor\n");
}

#[test]
fn run_logs_of_a_run_whose_actor_never_started_says_so() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let broken = home.write("broken.yaml", BROKEN_PROFILE);
  let failed = home.run(&["run", "create", "--profile", broken.to_str().unwrap()]);
  assert_eq!(failed.status.code(), Some(1));
  let run = stdout(&home.run(&["run", "ls", "-o", "json"]));
  let runs: Vec<serde_json::Value> = serde_json::from_str(&run).unwrap();
  let run = runs[0]["id"].as_str().unwrap().to_string();

  let output = home.run(&["run", "logs", &run]);

  assert_eq!(output.status.code(), Some(1));
  assert_eq!(stderr(&output), format!("needy: run {run} has no actor console\n"));
}

#[test]
fn profile_check_splits_quoted_commands_and_shows_matching_faults() {
  let home = Home::new();
  let profile = home.write("profile.yaml", NATIVE_PROFILE);

  let output = home.run(&[
    "profile",
    "check",
    "--local",
    profile.to_str().unwrap(),
    "--cmd",
    "curl 'http://example.com/a b'",
    "--cmd",
    "ls",
  ]);

  assert!(output.status.success(), "{}", stderr(&output));
  let text = stdout(&output);
  let curl = text.lines().find(|l| l.starts_with("curl ")).unwrap();
  assert!(curl.contains("executor here"), "{curl}");
  assert!(curl.contains("curl 'http://example.com/a b'  "), "argv is one word per argument: {curl}");
  assert!(curl.trim_end().ends_with("ECONNREFUSED"), "{curl}");
  let ls = text.lines().find(|l| l.starts_with("ls ")).unwrap();
  assert!(!ls.contains("ECONNREFUSED"), "{ls}");
}

/// Every example profile in the repository.
fn example_profiles() -> Vec<PathBuf> {
  let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
  let mut profiles = Vec::new();
  for entry in std::fs::read_dir(&examples).unwrap() {
    for file in std::fs::read_dir(entry.unwrap().path()).unwrap() {
      let path = file.unwrap().path();
      let name = path.file_name().unwrap().to_str().unwrap();
      if name.ends_with("profile.yaml") {
        profiles.push(path);
      }
    }
  }
  profiles.sort();
  profiles
}

#[test]
fn profile_check_accepts_every_example_profile() {
  let home = Home::new();
  let profiles = example_profiles();
  assert!(profiles.len() >= 8, "{profiles:?}");

  for profile in profiles {
    let output = home.run(&["profile", "check", "--local", profile.to_str().unwrap()]);

    assert!(output.status.success(), "{}: {}{}", profile.display(), stdout(&output), stderr(&output));
    assert!(!stdout(&output).contains("error:"), "{}: {}", profile.display(), stdout(&output));
  }
}

#[test]
fn profile_check_reports_unknown_names_and_exits_1() {
  let home = Home::new();
  let profile = home.write(
    "profile.yaml",
    "executors: { build: {} }\ncommands:\n  - { match: \"git **\", to: biuld }\n  - { match: \"make **\", use: [fast] }\n",
  );

  let output = home.run(&["profile", "check", "--local", profile.to_str().unwrap()]);

  assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
  let text = stdout(&output);
  assert!(text.contains("error: commands[0] (git **) sends to unknown executor \"biuld\"\n"), "{text}");
  assert!(text.contains("error: commands[1] (make **) uses unknown preset \"fast\"\n"), "{text}");
}

// Captures used to sit directly under `when`, where a misspelt condition
// became a capture that never matched. They now go under `captures:`.
#[test]
fn profile_check_rejects_flattened_captures() {
  let home = Home::new();
  let profile = home.write("profile.yaml", "commands: [{ match: \"node {script}\", when: { script: \"**/cli.js\" }, to: actor }]\n");

  let output = home.run(&["profile", "check", profile.to_str().unwrap()]);

  assert_eq!(output.status.code(), Some(1));
  let message = stderr(&output);
  assert!(message.starts_with(&format!("needy: invalid profile {}: ", profile.display())), "{message}");
  assert!(message.contains("commands[0].when.script: unknown field `script`, expected one of"), "{message}");
}

#[test]
fn profile_check_compiles_a_built_in_recipe() {
  let home = Home::new();
  let profile = home.write("profile.yaml", "actor: { recipe: \"builtin:gce-ubuntu-22.04\" }\n");

  let output = home.run(&["profile", "check", "--local", profile.to_str().unwrap()]);

  // The host CPU decides whether the recipe has findings, so only the
  // compiled machine is checked here.
  let text = stdout(&output);
  assert!(text.contains("recipe: 2 vCPU, 4096 MiB"), "{text}{}", stderr(&output));
  assert!(text.contains("  smbios: sys_vendor=Google, product_name=Google Compute Engine,"), "{text}");
}

#[test]
fn profile_check_asks_the_control_plane_where_the_profile_would_run() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let profile = home.write("profile.yaml", NATIVE_PROFILE);

  let output = home.run(&["profile", "check", profile.to_str().unwrap(), "--cmd", "ls"]);

  assert!(output.status.success(), "{}", stderr(&output));
  let text = stdout(&output);
  assert!(text.contains("\nactor: the control host\n"), "{text}");
  assert!(text.contains("\nexecutors: here (the control host)\n"), "{text}");
  assert!(text.lines().any(|l| l.starts_with("ls ") && l.contains("executor here")), "{text}");
}

#[test]
fn profile_check_through_the_control_plane_names_a_runner_nobody_joined_as_and_exits_1() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let profile = home.write("profile.yaml", "executors: { mac: { runner: mac } }\nactor: { runner: { labels: [kvm] } }\n");

  let output = home.run(&["profile", "check", profile.to_str().unwrap()]);

  assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
  let text = stdout(&output);
  assert!(text.contains("\nactor: unresolved\n"), "{text}");
  assert!(text.contains("\nexecutors: mac (unresolved)\n"), "{text}");
  assert!(text.contains("error: actor: no connected runner with the actor role has labels kvm; no runner has joined\n"), "{text}");
  assert!(text.contains("error: executors.mac: no runner is named mac; no runner has joined\n"), "{text}");
}

// Runs without an actor were removed with slice 5 of the runners-and-runs
// design; `needy exec --runner` runs commands without one.
#[test]
fn run_create_has_no_no_actor_flag() {
  let home = Home::new();

  let output = home.run(&["run", "create", "--help"]);

  assert!(output.status.success(), "{}", stderr(&output));
  assert!(!stdout(&output).contains("--no-actor"), "{}", stdout(&output));
}

#[test]
fn run_create_explains_profile_and_group() {
  let home = Home::new();

  let output = home.run(&["run", "create", "--help"]);

  let text = stdout(&output);
  let help = |flag: &str| text.lines().find(|l| l.trim_start().starts_with(flag)).unwrap().split_once(flag).unwrap().1.trim().to_string();
  assert!(help("--profile <PROFILE>").starts_with("Profile YAML"), "{text}");
  assert!(help("--group <GROUP>").starts_with("Runs in one group share executors"), "{text}");
  assert!(!help("--group <GROUP>").contains("[default"), "{text}");
}

// The runners-and-runs design renamed *session* to *run* without a
// compatibility alias (principle 7 of the module layout redesign).
#[test]
fn the_session_command_is_gone() {
  let home = Home::new();

  let output = home.run(&["session", "ls"]);

  assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
  assert!(stderr(&output).contains("unrecognized subcommand 'session'"), "{}", stderr(&output));
}

#[test]
fn state_directory_follows_needy_state_dir() {
  let home = Home::new();

  let output = home.run(&["report"]);

  assert_eq!(output.status.code(), Some(1));
  let expected = home.path().join("state").join("trace.db");
  assert!(stderr(&output).contains(expected.to_str().unwrap()), "{}", stderr(&output));
}

#[test]
fn state_directory_defaults_to_xdg_state_home() {
  let home = Home::new();

  let output = home.needy().env_remove("NEEDY_STATE_DIR").env("XDG_STATE_HOME", home.path().join("xdg")).arg("report").output().unwrap();

  let expected = home.path().join("xdg").join("needy").join("trace.db");
  assert!(stderr(&output).contains(expected.to_str().unwrap()), "{}", stderr(&output));
}

#[test]
fn socket_is_in_the_state_directory_without_a_runtime_directory() {
  let home = Home::new();

  let output = home.needy().env_remove("NEEDY_RUNTIME_DIR").arg("ps").output().unwrap();

  assert_eq!(output.status.code(), Some(1));
  let socket = home.path().join("state").join("daemon.sock");
  assert!(stderr(&output).contains(&format!("unix:{}", socket.display())), "{}", stderr(&output));
}

/// Sends signal `name` to process `pid`.
fn kill(pid: u32, name: &str) {
  let status = Command::new("kill").args([&format!("-{name}"), &pid.to_string()]).status().unwrap();
  assert!(status.success(), "kill -{name} {pid}");
}

/// A daemon in `mode` on the home's socket whose stderr the test reads.
fn spawn_daemon(home: &Home, mode: &str) -> Child {
  let mut command = home.needy();
  command.args(["daemon", "--mode", mode]);
  if mode == "control" {
    command.args(stand_in_actor(home));
  }
  let daemon = command.stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
  let socket = home.path().join("run").join("daemon.sock");
  let deadline = Instant::now() + Duration::from_secs(20);
  while !socket.exists() {
    assert!(Instant::now() < deadline, "the daemon did not create {}", socket.display());
    std::thread::sleep(Duration::from_millis(50));
  }
  daemon
}

/// A `sleep 300` run through `needy <exec>` and attached: an open stream,
/// which a graceful shutdown waits for. Returns the attached `needy` and
/// the sleep's process ID, once the sleep runs.
fn attach_sleep(home: &Home, exec: &[&str]) -> (Child, u32) {
  let pid_file = home.path().join("sleep.pid");
  let script = format!("echo $$ > {}; exec sleep 300", pid_file.display());
  let attached = home.needy().args(exec).args(["--", "sh", "-c", &script]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
  let deadline = Instant::now() + Duration::from_secs(20);
  let read = || std::fs::read_to_string(&pid_file).unwrap_or_default().trim().parse::<u32>().ok();
  while read().is_none() {
    assert!(Instant::now() < deadline, "the attached sleep did not start");
    std::thread::sleep(Duration::from_millis(50));
  }
  (attached, read().unwrap())
}

/// The output of an attached `needy` once it exits by itself.
fn exited(mut attached: Child) -> Output {
  let deadline = Instant::now() + Duration::from_secs(20);
  while attached.try_wait().unwrap().is_none() {
    if Instant::now() >= deadline {
      attached.kill().ok();
      panic!("the attached needy did not exit: {:?}", attached.wait_with_output().unwrap());
    }
    std::thread::sleep(Duration::from_millis(20));
  }
  attached.wait_with_output().unwrap()
}

// ROOT CAUSE:
//
// If an exec stream ended before the command exited, `needy exec` reported
// exit 0, because the relay started from a successful exit and kept it
// when no exit came.
//
// Before the fix, stopping the run of an attached `needy exec` made it
// exit 0 without a word.
// The fix fails such an exec with 255, saying why the stream ended.
#[test]
fn exec_whose_run_stops_exits_255_and_says_why() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let run = create_run(&home, NATIVE_PROFILE);
  let (attached, sleep) = attach_sleep(&home, &["exec", "--run", &run]);

  assert!(home.run(&["run", "stop", &run]).status.success());
  let output = exited(attached);
  // Leaving the stream ends the sleep; this is for a failed assertion only.
  let _ = Command::new("kill").args(["-KILL", &sleep.to_string()]).status();

  assert_eq!(output.status.code(), Some(255), "{}", stderr(&output));
  assert_eq!(stderr(&output), "needy: the connection ended before the command exited: run stopped\n");
}

#[test]
fn exec_json_whose_run_stops_reports_an_error_instead_of_an_exit() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let run = create_run(&home, NATIVE_PROFILE);
  let (attached, _) = attach_sleep(&home, &["exec", "--output", "json", "--run", &run]);

  assert!(home.run(&["run", "stop", &run]).status.success());
  let output = exited(attached);

  assert_eq!(output.status.code(), Some(255), "{}", stderr(&output));
  let summary: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
  assert_eq!(summary["error"], "the connection ended before the command exited: run stopped", "{summary}");
  assert!(summary.get("exit_code").is_none(), "{summary}");
}

#[test]
fn exec_whose_daemon_shuts_down_exits_255_and_says_why() {
  let home = Home::new();
  let mut daemon = spawn_daemon(&home, "control");
  let (attached, sleep) = attach_sleep(&home, &["exec"]);

  kill(daemon.id(), "TERM");
  let output = exited(attached);
  daemon.wait().unwrap();
  let _ = Command::new("kill").args(["-KILL", &sleep.to_string()]).status();

  assert_eq!(output.status.code(), Some(255), "{}", stderr(&output));
  assert_eq!(stderr(&output), "needy: the connection ended before the command exited: daemon shutting down\n");
}

/// A daemon whose exec streams end before their command starts: with
/// `reason`, the way a daemon says why; without one, by closing. It stops
/// when dropped.
struct EndingDaemon {
  endpoint: String,
  _runtime: tokio::runtime::Runtime,
}

impl EndingDaemon {
  fn start(home: &Home, reason: Option<&str>) -> Self {
    use needy_api_client::proto::v1::process_service_server::{ProcessService, ProcessServiceServer};
    use needy_api_client::proto::v1::{
      ExecEnded, ExecRequest, ExecResponse, KillProcessRequest, KillProcessResponse, ListProcessesRequest, ListProcessesResponse,
      exec_response,
    };
    use tonic::{Request, Response, Status, Streaming};

    struct Ending(Option<String>);

    #[tonic::async_trait]
    impl ProcessService for Ending {
      type ExecStream = tokio_stream::Iter<std::vec::IntoIter<Result<ExecResponse, Status>>>;

      async fn exec(&self, _: Request<Streaming<ExecRequest>>) -> Result<Response<Self::ExecStream>, Status> {
        let ended = self.0.iter().map(|reason| {
          Ok(ExecResponse {
            payload: Some(exec_response::Payload::Ended(ExecEnded {
              reason: reason.clone(),
            })),
          })
        });
        Ok(Response::new(tokio_stream::iter(ended.collect::<Vec<_>>())))
      }

      async fn list_processes(&self, _: Request<ListProcessesRequest>) -> Result<Response<ListProcessesResponse>, Status> {
        Err(Status::unimplemented("not part of the test"))
      }

      async fn kill_process(&self, _: Request<KillProcessRequest>) -> Result<Response<KillProcessResponse>, Status> {
        Err(Status::unimplemented("not part of the test"))
      }
    }

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let socket = home.path().join("ending.sock");
    let listener = {
      let _context = runtime.enter();
      tokio::net::UnixListener::bind(&socket).unwrap()
    };
    runtime.spawn(
      tonic::transport::Server::builder()
        .add_service(ProcessServiceServer::new(Ending(reason.map(str::to_string))))
        .serve_with_incoming(tokio_stream::wrappers::UnixListenerStream::new(listener)),
    );
    Self {
      endpoint: format!("unix:{}", socket.display()),
      _runtime: runtime,
    }
  }
}

// ROOT CAUSE:
//
// If the exec stream of `needy exec -d` ended before the process started,
// `needy` failed as if it had a bug of its own, because the detached path
// read the end as an error instead of an unknown outcome.
//
// Before the fix, it exited 1, with "the stream ended before the process
// started" or the daemon's reason worded for a command that had started.
// The fix exits 255 and says why the stream ended, like an exec whose stream
// ends before the command exits.
#[test]
fn detached_exec_whose_stream_ends_before_the_start_exits_255_and_says_why() {
  let home = Home::new();
  let daemon = EndingDaemon::start(&home, Some("run stopped"));

  let output = home.run(&[
    "--endpoint",
    &daemon.endpoint,
    "exec",
    "-d",
    "--",
    "sleep",
    "30",
  ]);

  assert_eq!(output.status.code(), Some(255), "{}", stderr(&output));
  assert_eq!(stdout(&output), "");
  assert_eq!(stderr(&output), "needy: the connection ended before the command started: run stopped\n");
}

#[test]
fn detached_exec_json_whose_stream_closes_before_the_start_reports_an_error() {
  let home = Home::new();
  let daemon = EndingDaemon::start(&home, None);

  let output = home.run(&[
    "--endpoint",
    &daemon.endpoint,
    "exec",
    "-d",
    "--output",
    "json",
    "--",
    "sleep",
    "30",
  ]);

  assert_eq!(output.status.code(), Some(255), "{}", stderr(&output));
  let summary: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
  assert_eq!(summary["error"], "the connection ended before the command started", "{summary}");
  assert!(summary.get("process_id").is_none(), "{summary}");
}

/// Sends SIGTERM twice, half a second apart, and returns how long the
/// daemon took to exit after the second and its output.
fn terminate_twice(mut daemon: Child) -> (Duration, Output) {
  kill(daemon.id(), "TERM");
  std::thread::sleep(Duration::from_millis(500));
  assert!(daemon.try_wait().unwrap().is_none(), "the first SIGTERM must start a graceful shutdown, not end it");
  kill(daemon.id(), "TERM");
  let second = Instant::now();
  let deadline = second + Duration::from_secs(20);
  while daemon.try_wait().unwrap().is_none() {
    assert!(Instant::now() < deadline, "the daemon did not exit");
    std::thread::sleep(Duration::from_millis(20));
  }
  let elapsed = second.elapsed();
  (elapsed, daemon.wait_with_output().unwrap())
}

// ROOT CAUSE:
//
// If a second SIGTERM came while the control daemon shut down, it was
// ignored, because the signal handlers stayed installed after the first
// signal and nothing listened for another.
//
// Before the fix, the daemon waited out its grace period for open streams
// and the shutdown, then exited 0.
// The fix keeps the first signal graceful and makes a second one exit at
// once, non-zero, naming the runs it did not clean up.
#[test]
fn a_second_sigterm_makes_the_control_daemon_exit_at_once() {
  let home = Home::new();
  let daemon = spawn_daemon(&home, "control");
  let run = create_run(&home, NATIVE_PROFILE);
  let (mut attached, sleep) = attach_sleep(&home, &["exec", "--run", &run]);

  let (elapsed, output) = terminate_twice(daemon);
  attached.kill().ok();
  attached.wait().unwrap();
  // A forced exit leaves the sleep behind.
  kill(sleep, "KILL");

  assert!(elapsed < Duration::from_secs(2), "the daemon took {elapsed:?} to exit after the second SIGTERM");
  assert_eq!(output.status.code(), Some(1));
  assert!(stderr(&output).contains(&run), "the exit names the run it left: {}", stderr(&output));
}

#[test]
fn a_second_sigterm_makes_the_executor_exit_at_once() {
  let home = Home::new();
  let daemon = spawn_daemon(&home, "executor");
  let (mut attached, sleep) = attach_sleep(&home, &["exec"]);

  let (elapsed, output) = terminate_twice(daemon);
  attached.kill().ok();
  attached.wait().unwrap();
  // A forced exit leaves the sleep behind.
  kill(sleep, "KILL");

  assert!(elapsed < Duration::from_secs(2), "the executor took {elapsed:?} to exit after the second SIGTERM");
  assert_eq!(output.status.code(), Some(1));
  assert!(stderr(&output).contains("these processes were not killed"), "{}", stderr(&output));
}

/// The table rows `needy ps` prints, split into cells by two or more spaces.
fn table(text: &str) -> Vec<Vec<String>> {
  text.lines().map(|line| line.split("  ").map(str::trim).filter(|c| !c.is_empty()).map(str::to_string).collect()).collect()
}

/// A native run with one `needy exec` and one `needy fs cp`, whose
/// `mkdir` and `tar` are needy's own processes.
fn run_with_internal_processes(home: &Home) -> String {
  let run = create_run(home, NATIVE_PROFILE);
  let exec = home.run(&["exec", "--run", &run, "--", "echo", "mine"]);
  assert!(exec.status.success(), "{}", stderr(&exec));
  let file = home.write("copied.txt", "copied\n");
  let dir = home.path().join("into");
  let cp = home.run(&[
    "fs",
    "cp",
    "--run",
    &run,
    file.to_str().unwrap(),
    &format!("here:{}", dir.display()),
  ]);
  assert!(cp.status.success(), "{}", stderr(&cp));
  run
}

// The owner's 2026-11-10 trial found that `needy ps` listed needy's own
// helpers (the `mkdir` and `tar` of `fs cp`) next to the user's commands,
// with no way to tell which run or caller started a process.
#[test]
fn ps_shows_run_and_origin_and_hides_needys_own_processes() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let run = run_with_internal_processes(&home);

  let output = home.run(&["ps", "--run", &run]);

  assert!(output.status.success(), "{}", stderr(&output));
  let rows = table(&stdout(&output));
  assert_eq!(rows[0][..3], ["ID", "RUN", "ORIGIN"]);
  assert_eq!(rows.len(), 2, "{rows:?}");
  assert_eq!(rows[1][1..3], [run.as_str(), "cli"]);
  assert_eq!(rows[1].last().unwrap(), "echo mine");
}

#[test]
fn ps_all_shows_needys_own_processes_with_their_origin() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let run = run_with_internal_processes(&home);

  let output = home.run(&["ps", "-a", "--run", &run]);

  assert!(output.status.success(), "{}", stderr(&output));
  let rows = table(&stdout(&output));
  let commands: Vec<(&str, &str)> = rows[1..].iter().map(|r| (r[2].as_str(), r.last().unwrap().as_str())).collect();
  assert_eq!(commands.len(), 3, "{rows:?}");
  assert_eq!(commands[0], ("cli", "echo mine"));
  assert!(commands[1].0 == "fs" && commands[1].1.starts_with("mkdir -p"), "{rows:?}");
  assert!(commands[2].0 == "fs" && commands[2].1.starts_with("tar -x"), "{rows:?}");
  assert!(rows[1..].iter().all(|r| r[1] == run), "{rows:?}");
}

#[test]
fn ps_json_carries_run_and_origin_and_respects_all() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let run = run_with_internal_processes(&home);
  let json = |args: &[&str]| -> Vec<serde_json::Value> {
    let output = home.run(args);
    assert!(output.status.success(), "{}", stderr(&output));
    serde_json::from_str(&stdout(&output)).unwrap()
  };

  let mine = json(&["ps", "-o", "json", "--run", &run]);
  let all = json(&["ps", "-o", "json", "--all", "--run", &run]);

  assert_eq!(mine.len(), 1, "{mine:?}");
  assert_eq!((mine[0]["run"].as_str(), mine[0]["origin"].as_str()), (Some(run.as_str()), Some("cli")));
  let origins: Vec<&str> = all.iter().map(|p| p["origin"].as_str().unwrap()).collect();
  assert_eq!(origins, ["cli", "fs", "fs"]);
}

/// Starts `command` detached in `run` and returns its process ID.
fn detach(home: &Home, run: &str, command: &[&str]) -> String {
  let output = home.needy().args(["exec", "-d", "--run", run, "--"]).args(command).output().unwrap();
  assert!(output.status.success(), "{}", stderr(&output));
  stdout(&output).trim().to_string()
}

/// Waits until `needy ps` no longer lists process `id` as running.
fn wait_until_ended(home: &Home, run: &str, id: &str) {
  let deadline = Instant::now() + Duration::from_secs(20);
  loop {
    let listed: Vec<serde_json::Value> = serde_json::from_str(&stdout(&home.run(&["ps", "-o", "json", "--run", run]))).unwrap();
    if listed.iter().any(|p| p["id"] == id && p["state"] != "running") {
      return;
    }
    assert!(Instant::now() < deadline, "process {id} did not end: {listed:?}");
    std::thread::sleep(Duration::from_millis(50));
  }
}

// The owner's 2026-11-10 trial: `ps` folded the exit into STATE (`exit 0`,
// `SIGINT`), so neither column could be filtered or sorted on its own.
#[test]
fn ps_shows_state_and_exit_in_separate_columns() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let run = create_run(&home, NATIVE_PROFILE);
  let running = detach(&home, &run, &["sleep", "30"]);
  let exited = detach(&home, &run, &["sh", "-c", "exit 3"]);
  let killed = detach(&home, &run, &["sleep", "31"]);
  assert!(home.run(&["kill", &killed, "--signal", "SIGINT"]).status.success());
  wait_until_ended(&home, &run, &exited);
  wait_until_ended(&home, &run, &killed);

  let output = home.run(&["ps", "--run", &run]);

  let text = stdout(&output);
  let header: Vec<&str> = text.lines().next().unwrap().split_whitespace().collect();
  assert_eq!(
    header,
    [
      "ID", "RUN", "ORIGIN", "EXECUTOR", "PID", "STATE", "EXIT", "TTY", "COMMAND"
    ]
  );
  let row = |id: &str| -> (String, String) {
    let line = text.lines().find(|l| l.starts_with(id)).unwrap_or_else(|| panic!("{text}"));
    let state_at = text.lines().next().unwrap().find("STATE").unwrap();
    let exit_at = text.lines().next().unwrap().find("EXIT").unwrap();
    let tty_at = text.lines().next().unwrap().find("TTY").unwrap();
    (line[state_at..exit_at].trim().to_string(), line[exit_at..tty_at].trim().to_string())
  };
  assert_eq!(row(&running), ("running".into(), "".into()));
  assert_eq!(row(&exited), ("exited".into(), "3".into()));
  assert_eq!(row(&killed), ("killed".into(), "SIGINT".into()));
  assert!(home.run(&["kill", &running, "--signal", "SIGKILL"]).status.success());
}

#[test]
fn ps_json_has_separate_state_exit_code_and_signal() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let run = create_run(&home, NATIVE_PROFILE);
  let exited = detach(&home, &run, &["sh", "-c", "exit 3"]);
  let killed = detach(&home, &run, &["sleep", "31"]);
  assert!(home.run(&["kill", &killed, "--signal", "SIGINT"]).status.success());
  wait_until_ended(&home, &run, &exited);
  wait_until_ended(&home, &run, &killed);

  let output = home.run(&["ps", "-o", "json", "--run", &run]);

  let listed: Vec<serde_json::Value> = serde_json::from_str(&stdout(&output)).unwrap();
  let fields = |id: &str| {
    let p = listed.iter().find(|p| p["id"] == id).unwrap();
    (p["state"].clone(), p["exit_code"].clone(), p["signal"].clone())
  };
  assert_eq!(fields(&exited), (serde_json::json!("exited"), serde_json::json!(3), serde_json::Value::Null));
  assert_eq!(fields(&killed), (serde_json::json!("killed"), serde_json::Value::Null, serde_json::json!("SIGINT")));
}

#[test]
fn attach_refuses_needys_own_processes() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let run = run_with_internal_processes(&home);
  let listed: Vec<serde_json::Value> = serde_json::from_str(&stdout(&home.run(&["ps", "-a", "-o", "json", "--run", &run]))).unwrap();
  let tar = listed.iter().find(|p| p["argv"][0] == "tar").unwrap()["id"].as_str().unwrap().to_string();

  let output = home.run(&["attach", &tar]);

  assert_eq!(output.status.code(), Some(1));
  assert_eq!(stderr(&output), format!("needy: process {tar} is needy's own (origin fs); it cannot be attached\n"));
}

// The owner's 2026-11-10 trial: the default report was markdown, which
// reads poorly in a terminal.
#[test]
fn report_defaults_to_plain_text() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let profile = home.write("review.profile.yaml", NATIVE_PROFILE);
  let created = home.run(&["run", "create", "--profile", profile.to_str().unwrap()]);
  let run = stdout(&created).trim().to_string();
  assert!(home.run(&["exec", "--run", &run, "--", "sh", "-c", "exit 2"]).status.code() == Some(2));
  assert!(home.run(&["run", "stop", &run]).status.success());

  let output = home.run(&["report", &run]);

  assert!(output.status.success(), "{}", stderr(&output));
  let report = stdout(&output);
  let mut lines = report.lines();
  assert_eq!(lines.next(), Some(format!("Run       {run}  (profile review)").as_str()));
  assert_eq!(lines.next(), Some("State     stopped"));
  assert!(lines.next().is_some_and(|l| l.starts_with("Duration  ") && l.ends_with('s')), "{report}");
  assert!(report.contains("\nCommands\n  routed to here   1  (1 failed)\n"), "{report}");
  assert!(report.contains("\nTop commands\n  sh on here  1  slowest "), "{report}");
  assert!(!report.contains('\u{1b}'), "no colors when stdout is not a terminal: {report}");
}

#[test]
fn report_json_is_a_summary_and_file_writes_it() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let run = create_run(&home, NATIVE_PROFILE);
  assert!(home.run(&["run", "stop", &run]).status.success());
  let file = home.path().join("report.json");

  let output = home.run(&[
    "report",
    &run,
    "-o",
    "json",
    "--file",
    file.to_str().unwrap(),
  ]);

  assert!(output.status.success(), "{}", stderr(&output));
  assert_eq!(stdout(&output), "");
  let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
  assert_eq!((&json["run"], &json["state"]), (&serde_json::json!(run), &serde_json::json!("stopped")));
  assert_eq!(json["route_names"], serde_json::Value::Null, "no actor ran");
  assert_eq!(json["findings"], serde_json::json!([]));
}

/// `needy daemon` with `args`, given `timeout` to exit by itself: its
/// output, and whether it exited (a daemon still running is killed).
fn daemon_within(home: &Home, args: &[&str], timeout: Duration) -> (bool, Output) {
  let mut daemon = home.needy().arg("daemon").args(args).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
  let deadline = Instant::now() + timeout;
  while daemon.try_wait().unwrap().is_none() {
    if Instant::now() >= deadline {
      daemon.kill().unwrap();
      return (false, daemon.wait_with_output().unwrap());
    }
    std::thread::sleep(Duration::from_millis(20));
  }
  (true, daemon.wait_with_output().unwrap())
}

// ROOT CAUSE:
//
// If `needy daemon --listen HOST:PORT` named a non-loopback address and no
// token, it served every method to anyone who could reach the port, because
// the server accepted any token when it had none.
//
// Before the fix, both modes listened on `0.0.0.0` without a token.
// The fix refuses such a listener at once, naming the address and
// `--token-env`.
#[test]
fn a_daemon_refuses_a_network_listener_without_a_token() {
  let home = Home::new();
  for mode in ["executor", "control"] {
    let (exited, output) = daemon_within(&home, &["--mode", mode, "--listen", "0.0.0.0:0"], Duration::from_secs(10));

    assert!(exited, "the {mode} daemon listened on 0.0.0.0 without a token");
    assert_eq!(output.status.code(), Some(1), "{mode}: {}", stderr(&output));
    assert!(stderr(&output).contains("0.0.0.0:0"), "{mode}: {}", stderr(&output));
    assert!(stderr(&output).contains("--token-env"), "{mode}: {}", stderr(&output));
  }
}

// ROOT CAUSE:
//
// If the machine was busy, the test failed although the daemon warned,
// because it gave the daemon a fixed 2 s to log the warning.
//
// Before the fix, a slow start read the daemon's output before the warning.
// The fix waits for the warning's line itself, within a generous bound.
#[test]
fn a_daemon_on_a_loopback_address_without_a_token_starts_with_a_warning() {
  use std::io::BufRead as _;
  let home = Home::new();
  let mut daemon = home
    .needy()
    .args([
      "daemon",
      "-v",
      "--mode",
      "executor",
      "--listen",
      "127.0.0.1:0",
    ])
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
  // The daemon's log is read on another thread, so the bound below holds
  // even if the daemon logs nothing.
  let log = std::io::BufReader::new(daemon.stderr.take().unwrap());
  let (lines, read) = std::sync::mpsc::channel();
  std::thread::spawn(move || {
    for line in log.lines().map_while(Result::ok) {
      if lines.send(line).is_err() {
        break;
      }
    }
  });
  let deadline = Instant::now() + Duration::from_secs(60);
  let mut seen = Vec::new();
  let warning = loop {
    let left = deadline.saturating_duration_since(Instant::now());
    match read.recv_timeout(left) {
      Ok(line) if line.contains("without a token") => break Some(line),
      Ok(line) => seen.push(line),
      // The daemon exited, or the bound passed.
      Err(_) => break None,
    }
  };
  let running = daemon.try_wait().unwrap().is_none();
  if daemon.kill().is_ok() {
    daemon.wait().unwrap();
  }

  let warning = warning.unwrap_or_else(|| panic!("no warning; the daemon logged: {}", seen.join("\n")));
  assert!(warning.contains("WARN"), "{warning}");
  assert!(running, "the executor did not keep running after the warning: {}", seen.join("\n"));
}

#[test]
fn a_daemon_refuses_a_token_variable_that_is_not_set() {
  let home = Home::new();

  let (exited, output) = daemon_within(
    &home,
    &[
      "--mode",
      "executor",
      "--listen",
      "127.0.0.1:0",
      "--token-env",
      "NEEDY_TEST_UNSET_TOKEN",
    ],
    Duration::from_secs(10),
  );

  assert!(exited, "the executor listened without the token it was told to use");
  assert_eq!(output.status.code(), Some(1));
  assert!(stderr(&output).contains("NEEDY_TEST_UNSET_TOKEN"), "{}", stderr(&output));
}

/// The permission bits of `path`.
fn mode(path: &Path) -> u32 {
  use std::os::unix::fs::PermissionsExt;
  std::fs::metadata(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())).permissions().mode() & 0o7777
}

// ROOT CAUSE:
//
// If the daemon ran under a permissive umask, its directories and sockets
// were open to every local user, because they were created with the
// umask's default modes and the control socket accepts any client without
// a token.
//
// Before the fix, under `umask 000` the runtime directory was 0777 and the
// sockets 0777.
// The fix creates the directories 0700 and the sockets 0600 whatever the
// umask.
#[test]
fn the_daemons_directories_and_sockets_are_private_under_a_permissive_umask() {
  let home = Home::new();
  let needy = home.needy();
  let mut command = Command::new("sh");
  command.args([
    "-c",
    "umask 000; exec \"$0\" daemon --mode control \"$@\"",
    needy.get_program().to_str().unwrap(),
  ]);
  command.args(stand_in_actor(&home));
  for (name, value) in needy.get_envs() {
    match value {
      Some(value) => command.env(name, value),
      None => command.env_remove(name),
    };
  }
  let child = command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
  let socket = home.path().join("run").join("daemon.sock");
  let deadline = Instant::now() + Duration::from_secs(20);
  while !socket.exists() {
    assert!(Instant::now() < deadline, "the daemon did not create {}", socket.display());
    std::thread::sleep(Duration::from_millis(50));
  }
  let _daemon = Daemon { child };
  let run = create_run(&home, NATIVE_PROFILE);

  let state = home.path().join("state");
  let runtime = home.path().join("run");
  let modes = [
    (state.clone(), mode(&state)),
    (state.join("runs"), mode(&state.join("runs"))),
    (state.join("runs").join(&run), mode(&state.join("runs").join(&run))),
    (runtime.clone(), mode(&runtime)),
    (runtime.join("daemon.sock"), mode(&runtime.join("daemon.sock"))),
    (runtime.join("local.sock"), mode(&runtime.join("local.sock"))),
  ];
  let expected = [0o700, 0o700, 0o700, 0o700, 0o600, 0o600];
  for ((path, mode), expected) in modes.iter().zip(expected) {
    assert_eq!(*mode, expected, "{} is {mode:o}", path.display());
  }
}

// A control daemon on another machine cannot read the client's profile
// directory; the client sends the profile and its recipe resolved. Here the
// daemon runs in another working directory than the profile's.
#[test]
fn run_create_sends_a_profile_with_a_relative_parent_and_recipe() {
  let home = Home::new();
  let elsewhere = home.path().join("elsewhere");
  std::fs::create_dir_all(&elsewhere).unwrap();
  let child = home
    .needy()
    .args(["daemon", "--mode", "control"])
    .args(stand_in_actor(&home))
    .current_dir(&elsewhere)
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .unwrap();
  let socket = home.path().join("run").join("daemon.sock");
  let deadline = Instant::now() + Duration::from_secs(20);
  while !socket.exists() {
    assert!(Instant::now() < deadline, "the daemon did not create {}", socket.display());
    std::thread::sleep(Duration::from_millis(50));
  }
  let _daemon = Daemon { child };
  std::fs::create_dir_all(home.path().join("p")).unwrap();
  home.write("p/base.yaml", NATIVE_PROFILE);
  home.write("p/gce.recipe.yaml", "extends: builtin:gce-ubuntu-22.04\n");
  let profile = home.write("p/profile.yaml", "extends: base.yaml\nactor: { recipe: gce.recipe.yaml }\n");

  let output = home.run(&["run", "create", "--profile", profile.to_str().unwrap()]);

  assert!(output.status.success(), "{}", stderr(&output));
}

/// Runs `needy` with `args` on a new terminal of `rows` x `cols` and
/// returns everything it wrote there, once it exits.
fn on_terminal(home: &Home, args: &[&str], size: (u16, u16)) -> (Option<i32>, Vec<u8>) {
  on_terminal_with(home, args, size, |_, _| {})
}

/// [`on_terminal`], calling `meanwhile` with `needy`'s process ID and what
/// it wrote so far once it runs.
fn on_terminal_with(
  home: &Home,
  args: &[&str],
  (rows, cols): (u16, u16),
  meanwhile: impl FnOnce(u32, &std::sync::Mutex<Vec<u8>>),
) -> (Option<i32>, Vec<u8>) {
  use portable_pty::{CommandBuilder, PtySize, native_pty_system};
  use std::io::Read;
  let pair = native_pty_system()
    .openpty(PtySize {
      rows,
      cols,
      pixel_width: 0,
      pixel_height: 0,
    })
    .unwrap();
  let template = home.needy();
  let mut command = CommandBuilder::new(template.get_program());
  command.args(args);
  for (name, value) in template.get_envs() {
    match value {
      Some(value) => command.env(name, value),
      None => command.env_remove(name),
    }
  }
  let mut child = pair.slave.spawn_command(command).unwrap();
  drop(pair.slave);
  let mut reader = pair.master.try_clone_reader().unwrap();
  let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
  let sink = written.clone();
  std::thread::spawn(move || {
    let mut buf = [0u8; 4096];
    while let Ok(n) = reader.read(&mut buf) {
      if n == 0 {
        break;
      }
      sink.lock().unwrap().extend_from_slice(&buf[..n]);
    }
  });
  meanwhile(child.process_id().unwrap(), &written);
  let deadline = Instant::now() + Duration::from_secs(20);
  let status = loop {
    if let Some(status) = child.try_wait().unwrap() {
      break status;
    }
    if Instant::now() >= deadline {
      child.kill().ok();
      panic!("needy {args:?} did not exit: {:?}", String::from_utf8_lossy(&written.lock().unwrap()));
    }
    std::thread::sleep(Duration::from_millis(20));
  };
  // What the client wrote last is still in the terminal's buffer.
  std::thread::sleep(Duration::from_millis(300));
  let code = i32::try_from(status.exit_code()).ok();
  let bytes = written.lock().unwrap().clone();
  (code, bytes)
}

/// Whether `haystack` contains `needle` after position `from`.
fn contains_after(haystack: &[u8], from: usize, needle: &[u8]) -> bool {
  haystack[from..].windows(needle.len()).any(|w| w == needle)
}

// ROOT CAUSE:
//
// If a program run with `needy exec -t` turned on terminal modes and ended
// without turning them off, they stayed on in the user's shell, because the
// client restored only raw mode.
//
// Before the fix, the alternate screen, mouse reporting, bracketed paste,
// focus events, the kitty keyboard protocol and a hidden cursor leaked.
// The fix turns them all off when the client leaves the terminal.
#[test]
fn exec_on_a_terminal_turns_off_the_programs_terminal_modes_when_it_ends() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let modes = r"printf '\033[?1049h\033[?1000h\033[?1006h\033[?2004h\033[?1004h\033[>1u\033[?25lMODES-ON'";

  let (code, written) = on_terminal(&home, &["exec", "-t", "--", "sh", "-c", modes], (24, 80));

  assert_eq!(code, Some(0), "{:?}", String::from_utf8_lossy(&written));
  let after = written.windows(8).position(|w| w == b"MODES-ON").expect("the program's output") + 8;
  for (mode, off) in [
    ("alternate screen", &b"\x1b[?1049l"[..]),
    ("mouse reporting", b"\x1b[?1000l"),
    ("SGR mouse", b"\x1b[?1006l"),
    ("bracketed paste", b"\x1b[?2004l"),
    ("focus events", b"\x1b[?1004l"),
    ("kitty keyboard", b"\x1b[=0;1u"),
    ("hidden cursor", b"\x1b[?25h"),
  ] {
    assert!(contains_after(&written, after, off), "{mode} is not turned off: {:?}", String::from_utf8_lossy(&written[after..]));
  }
}

// The terminal note's G4: a client whose process ignores the forwarded
// SIGTERM leaves at a second one, and the terminal is restored then too.
#[test]
fn exec_on_a_terminal_restores_it_when_a_second_sigterm_makes_it_leave() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let ignores_term = r"trap '' TERM; printf '\033[?1049h\033[?2004hMODES-ON'; sleep 30";

  let (code, written) = on_terminal_with(&home, &["exec", "-t", "--", "sh", "-c", ignores_term], (24, 80), |pid, written| {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !written.lock().unwrap().windows(8).any(|w| w == b"MODES-ON") {
      assert!(Instant::now() < deadline, "the program did not start");
      std::thread::sleep(Duration::from_millis(20));
    }
    kill(pid, "TERM");
    std::thread::sleep(Duration::from_millis(300));
    kill(pid, "TERM");
  });

  assert_eq!(code, Some(143), "{:?}", String::from_utf8_lossy(&written));
  let after = written.windows(8).position(|w| w == b"MODES-ON").unwrap() + 8;
  assert!(contains_after(&written, after, b"\x1b[?1049l"), "{:?}", String::from_utf8_lossy(&written[after..]));
  assert!(contains_after(&written, after, b"\x1b[?2004l"), "{:?}", String::from_utf8_lossy(&written[after..]));
}

// The terminal note's G9: `COLORTERM` was not forwarded, and a `TERM` the
// executor has no terminfo entry for was passed on as it was.
#[test]
fn exec_on_a_terminal_forwards_colorterm_and_replaces_an_unknown_term() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);

  let output = home
    .needy()
    .env("TERM", "needy-no-such-terminal")
    .env("COLORTERM", "needy-test-colors")
    .args([
      "exec",
      "-t",
      "--",
      "sh",
      "-c",
      "echo \"[$TERM] [$COLORTERM]\"",
    ])
    .output()
    .unwrap();

  assert!(output.status.success(), "{}", stderr(&output));
  assert!(stdout(&output).contains("[xterm-256color] [needy-test-colors]"), "{:?}", stdout(&output));
}

// ------------------------------------------------------------- runners

/// `needy runner token create` with `args`: the token it printed.
fn create_join_token(home: &Home, args: &[&str]) -> String {
  let output = home.needy().args(["runner", "token", "create"]).args(args).output().unwrap();
  assert!(output.status.success(), "token create: {}", stderr(&output));
  stdout(&output).trim().to_string()
}

#[test]
fn runner_token_create_prints_the_token_once_and_ls_lists_it_without_the_secret() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);

  let token = create_join_token(
    &home,
    &[
      "--ttl",
      "1h",
      "--uses",
      "2",
      "--roles",
      "executor",
      "--labels",
      "os=darwin,arch=arm64",
    ],
  );
  let table = home.run(&["runner", "token", "ls"]);
  let json = home.run(&["runner", "token", "ls", "-o", "json"]);

  let id = token.split('.').nth(1).unwrap();
  assert!(token.starts_with("needy-join."), "{token}");
  assert!(!stdout(&json).contains(&token));
  let table = stdout(&table);
  let lines: Vec<&str> = table.lines().collect();
  assert_eq!(lines[0].split_whitespace().collect::<Vec<_>>(), ["ID", "STATE", "USES", "LEFT", "EXPIRES", "ROLES", "LABELS"]);
  let cells: Vec<&str> = lines[1].split_whitespace().collect();
  assert_eq!((cells[0], cells[1], cells[2]), (id, "valid", "2"));
  assert_eq!(&cells[cells.len() - 2..], ["executor", "arch=arm64,os=darwin"]);
  assert!(!table.contains(&token));
  let rows: serde_json::Value = serde_json::from_str(&stdout(&json)).unwrap();
  assert_eq!(rows[0]["id"], id);
  assert_eq!(rows[0]["state"], "valid");
  assert_eq!(rows[0]["roles"], serde_json::json!(["executor"]));
  assert_eq!(rows[0]["labels"]["os"], "darwin");
}

#[test]
fn runner_token_revoke_shows_it_revoked() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let token = create_join_token(&home, &[]);
  let id = token.split('.').nth(1).unwrap();

  let revoked = home.run(&["runner", "token", "revoke", id]);
  let listed = home.run(&["runner", "token", "ls", "-o", "json"]);
  let missing = home.run(&["runner", "token", "revoke", "nope"]);

  assert!(revoked.status.success(), "{}", stderr(&revoked));
  let rows: serde_json::Value = serde_json::from_str(&stdout(&listed)).unwrap();
  assert_eq!(rows[0]["state"], "revoked");
  assert_eq!(rows[0]["roles"], serde_json::json!([]), "a token without roles allows every role");
  assert_eq!(missing.status.code(), Some(1));
  assert_eq!(stderr(&missing), "needy: no join token nope\n");
}

#[test]
fn runner_token_create_refuses_an_unknown_role_and_a_malformed_label() {
  let home = Home::new();

  let role = home.run(&["runner", "token", "create", "--roles", "builder"]);
  let label = home.run(&["runner", "token", "create", "--labels", "os"]);

  assert_eq!(role.status.code(), Some(2));
  assert!(stderr(&role).contains("unknown runner role \"builder\""), "{}", stderr(&role));
  assert_eq!(label.status.code(), Some(2));
  assert!(stderr(&label).contains("a label is KEY=VALUE"), "{}", stderr(&label));
}

#[test]
fn runner_ls_without_runners_prints_the_header() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);

  let output = home.run(&["runner", "ls"]);

  assert!(output.status.success(), "{}", stderr(&output));
  assert_eq!(stdout(&output).split_whitespace().collect::<Vec<_>>(), ["NAME", "STATUS", "ROLES", "LABELS", "LAST", "SEEN"]);
}

#[test]
fn runner_rotate_and_rm_name_a_missing_runner() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);

  let rotate = home.run(&["runner", "rotate", "ghost"]);
  let rm = home.run(&["runner", "rm", "ghost"]);

  assert_eq!((rotate.status.code(), stderr(&rotate).as_str()), (Some(1), "needy: no runner ghost\n"));
  assert_eq!((rm.status.code(), stderr(&rm).as_str()), (Some(1), "needy: no runner ghost\n"));
}

/// A control daemon whose runners send a heartbeat every 200 ms.
fn spawn_control_with_fast_heartbeats(home: &Home) -> Daemon {
  let child = home
    .needy()
    .args([
      "daemon",
      "--mode",
      "control",
      "--heartbeat-interval",
      "200ms",
    ])
    .args(stand_in_actor(home))
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .unwrap();
  let socket = home.path().join("run").join("daemon.sock");
  let deadline = Instant::now() + Duration::from_secs(20);
  while !socket.exists() {
    assert!(Instant::now() < deadline, "the daemon did not create {}", socket.display());
    std::thread::sleep(Duration::from_millis(50));
  }
  Daemon { child }
}

/// `needy daemon --mode runner` in its own home, joining `control`.
fn spawn_runner(home: &Home, control: &Home, join_token: Option<&str>) -> Child {
  let endpoint = format!("unix:{}", control.path().join("run").join("daemon.sock").display());
  let mut command = home.needy();
  command.args([
    "daemon", "--mode", "runner", "--join", &endpoint, "--name", "mac", "--labels", "os=test",
  ]);
  if let Some(token) = join_token {
    command.args(["--join-token-env", "NEEDY_TEST_JOIN_TOKEN"]).env("NEEDY_TEST_JOIN_TOKEN", token);
  }
  command.stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap()
}

/// The runner `name` as `needy runner ls -o json` shows it, once `check`
/// holds for it.
fn runner_when(control: &Home, name: &str, check: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
  let deadline = Instant::now() + Duration::from_secs(20);
  loop {
    let output = control.run(&["runner", "ls", "-o", "json"]);
    let rows: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap_or_default();
    if let Some(row) = rows.as_array().into_iter().flatten().find(|r| r["name"] == name && check(r)) {
      return row.clone();
    }
    assert!(Instant::now() < deadline, "runner {name}: {}", stdout(&output));
    std::thread::sleep(Duration::from_millis(50));
  }
}

#[test]
fn a_runner_daemon_joins_restarts_without_its_join_token_and_ends_when_removed() {
  let control = Home::new();
  let _daemon = spawn_control_with_fast_heartbeats(&control);
  let token = create_join_token(&control, &["--roles", "executor"]);
  let machine = Home::new();

  let first = spawn_runner(&machine, &control, Some(&token));
  let joined = runner_when(&control, "mac", |r| r["status"] == "online");
  kill(first.id(), "TERM");
  exited(first);
  let second = spawn_runner(&machine, &control, None);
  let restarted = runner_when(&control, "mac", |r| r["last_seen_at_ms"].as_i64() > joined["last_seen_at_ms"].as_i64());
  let rm = control.run(&["runner", "rm", "mac"]);
  let refused = exited(second);

  assert_eq!(joined["labels"]["os"], "test");
  assert_eq!(joined["roles"], serde_json::json!(["executor"]));
  assert_eq!(restarted["status"], "online");
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(machine.path().join("state").join("runner.json")).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
  }
  assert!(rm.status.success(), "{}", stderr(&rm));
  assert_eq!(refused.status.code(), Some(1));
  assert!(stderr(&refused).contains("the control plane refused this runner's credential"), "{}", stderr(&refused));
  assert_eq!(runner_when(&control, "mac", |_| true)["status"], "removed");
}

#[test]
fn a_runner_daemon_with_an_expired_or_spent_join_token_is_refused() {
  let control = Home::new();
  let _daemon = Daemon::start(&control);
  let expired = create_join_token(&control, &["--ttl", "1ms"]);
  let spent = create_join_token(&control, &[]);
  let used = Home::new();
  let first = spawn_runner(&used, &control, Some(&spent));
  runner_when(&control, "mac", |r| r["status"] == "online");
  kill(first.id(), "TERM");
  exited(first);
  std::thread::sleep(Duration::from_millis(5));

  let late = exited(spawn_runner(&Home::new(), &control, Some(&expired)));
  let again = exited(spawn_runner(&Home::new(), &control, Some(&spent)));

  assert_eq!(late.status.code(), Some(1));
  assert!(stderr(&late).contains("expired"), "{}", stderr(&late));
  assert_eq!(again.status.code(), Some(1));
  assert!(stderr(&again).contains("has no uses left"), "{}", stderr(&again));
}

/// A runner daemon, killed when dropped.
struct RunnerDaemon(Child);

impl Drop for RunnerDaemon {
  fn drop(&mut self) {
    // The test is over; a runner that already exited needs no kill.
    if self.0.kill().is_ok() {
      self.0.wait().unwrap();
    }
  }
}

/// A runner `mac` joined to `control`, once `needy ps --runner mac` reaches
/// it through its control stream.
fn connected_runner(machine: &Home, control: &Home) -> RunnerDaemon {
  let token = create_join_token(control, &[]);
  let runner = RunnerDaemon(spawn_runner(machine, control, Some(&token)));
  let deadline = Instant::now() + Duration::from_secs(20);
  loop {
    let ps = control.run(&["ps", "--runner", "mac"]);
    if ps.status.success() {
      return runner;
    }
    assert!(Instant::now() < deadline, "the runner did not connect: {}", stderr(&ps));
    std::thread::sleep(Duration::from_millis(50));
  }
}

#[test]
fn exec_on_a_runner_returns_the_command_exit_code_and_output() {
  let control = Home::new();
  let _daemon = Daemon::start(&control);
  let machine = Home::new();
  let _runner = connected_runner(&machine, &control);

  let output = control.run(&[
    "exec",
    "-T",
    "--runner",
    "mac",
    "--",
    "sh",
    "-c",
    "echo hi; echo oops >&2; exit 3",
  ]);
  let missing = control.run(&["exec", "--runner", "mac", "--", "needy-no-such-command"]);
  let json = control.run(&[
    "exec",
    "--runner",
    "mac",
    "--output",
    "json",
    "--",
    "sh",
    "-c",
    "kill -KILL $$",
  ]);

  assert_eq!((output.status.code(), stdout(&output).as_str(), stderr(&output).as_str()), (Some(3), "hi\n", "oops\n"));
  assert_eq!(missing.status.code(), Some(127), "{}", stderr(&missing));
  let json: serde_json::Value = serde_json::from_str(&stdout(&json)).unwrap();
  assert_eq!((json["executor"].as_str(), json["signal"].as_str()), (Some("mac"), Some("SIGKILL")));
}

#[test]
fn a_detached_process_on_a_runner_is_listed_and_killed_and_files_copy_to_it() {
  let control = Home::new();
  let _daemon = Daemon::start(&control);
  let machine = Home::new();
  let _runner = connected_runner(&machine, &control);
  let source = control.write("hello.txt", "hello runner\n");
  let destination = machine.path().join("copied");

  let detached = control.run(&["exec", "-d", "--runner", "mac", "--", "sleep", "600"]);
  let id = stdout(&detached).trim().to_string();
  let listed = control.run(&["ps", "--runner", "mac", "-o", "json"]);
  let killed = control.run(&["kill", "-s", "SIGKILL", &id]);
  let copied = control.run(&[
    "fs",
    "cp",
    "--runner",
    source.to_str().unwrap(),
    &format!("mac:{}", destination.display()),
  ]);

  assert!(detached.status.success(), "{}", stderr(&detached));
  let rows: serde_json::Value = serde_json::from_str(&stdout(&listed)).unwrap();
  let row = rows.as_array().unwrap().iter().find(|r| r["id"] == id.as_str()).expect("listed");
  assert_eq!((row["executor"].as_str(), row["state"].as_str()), (Some("mac"), Some("running")));
  assert!(killed.status.success(), "{}", stderr(&killed));
  assert!(copied.status.success(), "{}", stderr(&copied));
  assert_eq!(std::fs::read_to_string(destination.join("hello.txt")).unwrap(), "hello runner\n");
}

#[test]
fn exec_on_a_runner_that_is_not_connected_fails_at_once() {
  let control = Home::new();
  let _daemon = Daemon::start(&control);
  let started = Instant::now();

  let exec = control.run(&["exec", "--runner", "ghost", "--", "true"]);
  let ps = control.run(&["ps", "--runner", "ghost"]);

  assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
  assert_eq!((exec.status.code(), stderr(&exec).as_str()), (Some(1), "needy: runner ghost is not connected\n"));
  assert_eq!((ps.status.code(), stderr(&ps).as_str()), (Some(1), "needy: runner ghost is not connected\n"));
}

#[test]
fn a_runner_listens_nowhere() {
  let home = Home::new();

  let output = home.run(&["daemon", "--mode", "runner", "--listen", "127.0.0.1:0"]);

  assert_eq!(output.status.code(), Some(1));
  assert!(stderr(&output).contains("a runner dials the control plane and listens nowhere"), "{}", stderr(&output));
}

#[test]
fn exec_names_a_runner_or_a_run_not_both() {
  let home = Home::new();

  let output = home.run(&["exec", "--runner", "mac", "--run", "r1", "--", "true"]);

  assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
}
