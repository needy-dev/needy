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

/// A control daemon on the home's default socket, stopped on drop.
struct Daemon {
  child: Child,
}

impl Daemon {
  fn start(home: &Home) -> Self {
    let child = home.needy().args(["daemon", "--mode", "control"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
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
  here: { runtime: native }
commands:
  - { match: "rm **", deny: no deleting here }
  - { match: "**", to: here }
chaos:
  faults:
    - { on: exec, match: "curl **", errno: ECONNREFUSED }
    - { on: exec, match: "sleep *", delay: 1ms }
"#;

fn create_session(home: &Home, profile: &str) -> String {
  let profile = home.write("profile.yaml", profile);
  let output = home.run(&[
    "session",
    "create",
    "--no-actor",
    "--profile",
    profile.to_str().unwrap(),
  ]);
  assert!(output.status.success(), "session create: {}", stderr(&output));
  stdout(&output).trim().to_string()
}

// The QA pass on 2026-11-09 found that an injected errno on the CLI path
// exited 1 with "needy: needy chaos: ECONNREFUSED"; the actor path exits 126.
#[test]
fn exec_fault_exits_126_and_says_fault_injected() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let session = create_session(&home, NATIVE_PROFILE);

  let output = home.run(&[
    "exec",
    "--session",
    &session,
    "--",
    "curl",
    "http://example.com",
  ]);

  assert_eq!(output.status.code(), Some(126));
  assert_eq!(stderr(&output), "needy: fault injected: ECONNREFUSED\n");
}

#[test]
fn exec_denied_by_a_rule_exits_126() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let session = create_session(&home, NATIVE_PROFILE);

  let output = home.run(&["exec", "--session", &session, "--", "rm", "-rf", "/"]);

  assert_eq!(output.status.code(), Some(126));
  assert_eq!(stderr(&output), "needy: denied: no deleting here\n");
}

#[test]
fn exec_fault_in_json_output_reports_the_exit_code() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let session = create_session(&home, NATIVE_PROFILE);

  let output = home.run(&[
    "exec",
    "--output",
    "json",
    "--session",
    &session,
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
  let session = create_session(&home, NATIVE_PROFILE);

  let delayed = home.run(&[
    "exec",
    "--output",
    "ndjson",
    "--session",
    &session,
    "--",
    "sleep",
    "0",
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
    "--session",
    &session,
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
fn exec_ndjson_without_a_session_has_no_decision() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);

  let output = home.run(&["exec", "--output", "ndjson", "--", "true"]);

  assert_eq!(types(&ndjson(&output)), ["started", "exit"]);
}

// ROOT CAUSE:
//
// If one session was running, an exec without `--session` was traced as that
// session's, and could meet its chaos faults, because the relay looked the
// session up by an empty ID prefix, which matches every session.
//
// Before the fix, the report of the running session listed the command.
// The fix keeps an exec without a session out of every session.
#[test]
fn exec_without_a_session_stays_out_of_a_running_session() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let session = create_session(&home, NATIVE_PROFILE);

  let output = home.run(&["exec", "--", "sh", "-c", "echo sessionless"]);
  assert_eq!(stdout(&output), "sessionless\n");
  assert!(home.run(&["session", "stop", &session]).status.success());

  let report = stdout(&home.run(&["report", &session, "--level", "detailed"]));
  assert!(report.contains("| session.stop |"), "{report}");
  assert!(!report.contains("sessionless"), "{report}");
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
fn session_create_names_a_missing_profile() {
  let home = Home::new();
  let missing = home.path().join("missing.yaml");

  let output = home.run(&[
    "session",
    "create",
    "--no-actor",
    "--profile",
    missing.to_str().unwrap(),
  ]);

  assert_eq!(output.status.code(), Some(1));
  assert!(stderr(&output).contains(missing.to_str().unwrap()), "{}", stderr(&output));
}

#[test]
fn session_create_refuses_a_profile_with_errors() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let profile = home.write("profile.yaml", "executors: { here: { runtime: native } }\ncommands: [{ match: \"**\", to: hier }]\n");

  let output = home.run(&[
    "session",
    "create",
    "--no-actor",
    "--profile",
    profile.to_str().unwrap(),
  ]);

  assert_eq!(output.status.code(), Some(1));
  let message = stderr(&output);
  assert!(message.contains("invalid profile: commands[0] (**) sends to unknown executor \"hier\""), "{message}");
}

#[test]
fn session_ls_shows_stopped_and_failed_sessions() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let stopped = create_session(&home, NATIVE_PROFILE);
  assert!(home.run(&["session", "stop", &stopped]).status.success());
  let broken = home.write("broken.yaml", "user: { name: agent, uid: 1000, gid: 1000 }\nactor: { recipe: missing.recipe.yaml }\n");
  let failed = home.run(&[
    "session",
    "create",
    "--no-actor",
    "--name",
    "broken",
    "--profile",
    broken.to_str().unwrap(),
  ]);
  assert_eq!(failed.status.code(), Some(1));

  let output = home.run(&["session", "ls", "--output", "json"]);

  assert!(output.status.success(), "{}", stderr(&output));
  let sessions: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
  let state =
    |key: &str, value: &str| sessions.as_array().unwrap().iter().find(|s| s[key] == value).map(|s| s["state"].as_str().unwrap().to_string());
  assert_eq!(state("id", &stopped).as_deref(), Some("stopped"));
  assert_eq!(state("name", "broken").as_deref(), Some("failed"));
}

// The QA pass on 2026-11-09 found that a failed create left nothing in the
// trace, so `needy report` could not say why the session failed.
#[test]
fn a_failed_session_create_records_session_fail_with_the_reason() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let broken = home.write("broken.yaml", "user: { name: agent, uid: 1000, gid: 1000 }\nactor: { recipe: missing.recipe.yaml }\n");
  let failed = home.run(&[
    "session",
    "create",
    "--no-actor",
    "--profile",
    broken.to_str().unwrap(),
  ]);
  assert_eq!(failed.status.code(), Some(1));

  let output = home.run(&["report", "latest"]);

  assert!(output.status.success(), "{}", stderr(&output));
  let report = stdout(&output);
  let overview = report.lines().skip_while(|l| !l.starts_with("| Duration")).nth(2).unwrap();
  assert!(overview.contains("| failed |"), "{report}");
  let finding = report.lines().find(|l| l.starts_with("- ❌ Session failed: ")).unwrap_or_else(|| panic!("{report}"));
  assert!(finding.contains("missing.recipe.yaml"), "{finding}");
}

// The QA pass on 2026-11-09 found that the report named a profile after its
// directory, because the session request carried no file name.
#[test]
fn report_names_the_profile_after_its_file() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let profile = home.write("review.profile.yaml", NATIVE_PROFILE);
  let created = home.run(&[
    "session",
    "create",
    "--no-actor",
    "--profile",
    profile.to_str().unwrap(),
  ]);
  assert!(created.status.success(), "{}", stderr(&created));
  let session = stdout(&created).trim().to_string();

  let output = home.run(&["report", &session]);

  assert!(output.status.success(), "{}", stderr(&output));
  let title = stdout(&output).lines().next().unwrap().to_string();
  assert_eq!(title, format!("# Session report: {session} (profile: review)"));
}

#[test]
fn session_stop_is_traced_without_attributes() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let session = create_session(&home, NATIVE_PROFILE);
  assert!(home.run(&["session", "stop", &session]).status.success());

  let output = home.run(&["report", &session, "--level", "detailed"]);

  assert!(output.status.success(), "{}", stderr(&output));
  let report = stdout(&output);
  let stop = report.lines().find(|l| l.contains("| session.stop |")).unwrap_or_else(|| panic!("{report}"));
  assert!(stop.ends_with("| session.stop | `{}` |"), "{stop}");
  assert!(report.lines().nth(2).is_some_and(|l| !l.contains("incomplete")), "{report}");
}

#[test]
fn session_ls_prints_a_table() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let session = create_session(&home, NATIVE_PROFILE);

  let output = home.run(&["session", "ls"]);

  let text = stdout(&output);
  let mut lines = text.lines();
  let header: Vec<&str> = lines.next().unwrap().split_whitespace().collect();
  assert_eq!(header, ["ID", "NAME", "STATE", "GROUP", "EXECUTORS"]);
  let row: Vec<&str> = lines.next().unwrap().split_whitespace().collect();
  assert_eq!(
    row,
    [
      session.as_str(),
      &session,
      "running",
      &session,
      "here=native"
    ]
  );
}

#[test]
fn session_logs_prints_the_actor_console() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let session = create_session(&home, NATIVE_PROFILE);
  // Firecracker writes the console here; this session has no actor, so the
  // test writes it.
  std::fs::write(home.path().join("state/sessions").join(&session).join("console.log"), "boot\nhello from the actor\n").unwrap();

  let output = home.run(&["session", "logs", &session]);

  assert!(output.status.success(), "{}", stderr(&output));
  assert_eq!(stdout(&output), "boot\nhello from the actor\n");
}

#[test]
fn session_logs_without_an_actor_says_so() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let session = create_session(&home, NATIVE_PROFILE);

  let output = home.run(&["session", "logs", &session]);

  assert_eq!(output.status.code(), Some(1));
  assert_eq!(stderr(&output), format!("needy: session {session} has no actor console\n"));
}

#[test]
fn profile_check_splits_quoted_commands_and_shows_matching_faults() {
  let home = Home::new();
  let profile = home.write("profile.yaml", NATIVE_PROFILE);

  let output = home.run(&[
    "profile",
    "check",
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
    let output = home.run(&["profile", "check", profile.to_str().unwrap()]);

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

  let output = home.run(&["profile", "check", profile.to_str().unwrap()]);

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

  let output = home.run(&["profile", "check", profile.to_str().unwrap()]);

  // The host CPU decides whether the recipe has findings, so only the
  // compiled machine is checked here.
  let text = stdout(&output);
  assert!(text.contains("recipe: 2 vCPU, 4096 MiB"), "{text}{}", stderr(&output));
  assert!(text.contains("  smbios: sys_vendor=Google, product_name=Google Compute Engine,"), "{text}");
}

#[test]
fn session_create_explains_profile_and_group() {
  let home = Home::new();

  let output = home.run(&["session", "create", "--help"]);

  let text = stdout(&output);
  let help = |flag: &str| text.lines().find(|l| l.trim_start().starts_with(flag)).unwrap().split_once(flag).unwrap().1.trim().to_string();
  assert!(help("--profile <PROFILE>").starts_with("Profile YAML"), "{text}");
  assert!(help("--group <GROUP>").starts_with("Sessions in one group share executors"), "{text}");
  assert!(!help("--group <GROUP>").contains("[default"), "{text}");
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
