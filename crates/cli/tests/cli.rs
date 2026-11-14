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
      "NEEDY_EXECUTION_CONTEXT",
      "NEEDY_TOKEN",
      "NEEDY_RUN",
      "XDG_CONFIG_HOME",
      "XDG_STATE_HOME",
      "XDG_DATA_HOME",
      "XDG_RUNTIME_DIR",
    ] {
      command.env_remove(name);
    }
    command
      .env("NEEDY_CONFIG_DIR", self.path().join("config"))
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
  let image = home.path().join("image");
  std::fs::create_dir_all(&image).unwrap();
  std::fs::write(image.join("vmlinux"), "").unwrap();
  std::fs::write(image.join("rootfs.ext4"), "").unwrap();
  std::fs::write(
    image.join("manifest.json"),
    r#"{"version":"0.0.1","driver":"linux-firecracker","arch":"x86_64","base_abi":1,"source":{"kind":"build"}}"#,
  )
  .unwrap();
  let firecracker = image.join("firecracker");
  let script = "#!/bin/sh\necho boot\necho hello from the actor\nwhile kill -0 $PPID 2>/dev/null; do sleep 0.05; done\n";
  needy_common::testing::write_executable(&firecracker, script).unwrap();
  let kvm = home.path().join("kvm");
  std::fs::write(&kvm, "").unwrap();
  let guest = home.path().join("needy-linux");
  std::fs::write(&guest, static_elf()).unwrap();
  vec![
    "--actor-image".into(),
    image.display().to_string(),
    "--kvm".into(),
    kvm.display().to_string(),
    "--guest-needy".into(),
    guest.display().to_string(),
  ]
}

/// What the daemon checks of a guest needy: an ELF file header and one
/// loadable segment of a static Linux executable for this machine's
/// architecture. The stand-in actor never runs it.
// Source: the ELF-64 Object File Format, version 1.5 draft 2 (1998),
// "ELF Header" and "Program header table", https://uclibc.org/docs/elf-64-gen.pdf;
// `EM_X86_64` (62) and `EM_AARCH64` (183) from the System V gABI's machine
// table, https://www.sco.com/developers/gabi/latest/ch4.eheader.html
fn static_elf() -> Vec<u8> {
  let machine: u16 = if cfg!(target_arch = "aarch64") {
    183
  } else {
    62
  };
  let mut elf = vec![0x7f, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0];
  elf.extend(2u16.to_le_bytes());
  elf.extend(machine.to_le_bytes());
  elf.extend(1u32.to_le_bytes());
  for word in [0x40_1000u64, 64, 0] {
    elf.extend(word.to_le_bytes());
  }
  elf.extend(0u32.to_le_bytes());
  for half in [64u16, 56, 1, 64, 0, 0] {
    elf.extend(half.to_le_bytes());
  }
  elf.extend(1u32.to_le_bytes());
  elf.extend(5u32.to_le_bytes());
  for word in [0u64, 0x40_0000, 0x40_0000, 120, 120, 0x1000] {
    elf.extend(word.to_le_bytes());
  }
  elf
}

/// A control daemon on the home's default socket, stopped on drop.
struct Daemon {
  child: Child,
}

impl Daemon {
  fn start(home: &Home) -> Self {
    let child =
      home.needy().args(["daemon", "serve"]).args(stand_in_actor(home)).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let socket = home.path().join("run").join("daemon.sock");
    let deadline = Instant::now() + Duration::from_secs(20);
    // Publication follows health and reconciliation; a listening socket alone
    // does not mean local discovery is ready for a command.
    while std::os::unix::net::UnixStream::connect(&socket).is_err() || !home.path().join("run/control.json").exists() {
      assert!(Instant::now() < deadline, "the daemon did not listen on {}", socket.display());
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

/// Runs every command on this machine, refuses `rm`, and ends `curl` as a
/// refused connection would.
const NATIVE_PROFILE: &str = r#"
user: { name: agent, uid: 1000, gid: 1000 }
actor: { driver: linux-firecracker }
executors:
  here: { driver: native }
commands:
  - { match: "rm **", deny: no deleting here }
  - { match: "**", to: here }
chaos:
  faults:
    curl-down: { exec: "curl **", exit: 7, stderr: "curl: (7) Failed to connect to example.com port 80\n" }
    slow-sleep: { exec: "sleep *", delay: 1ms }
"#;

/// A profile the daemon refuses when it creates the run: its default
/// executor does not exist.
const BROKEN_PROFILE: &str =
  "user: { name: agent, uid: 1000, gid: 1000 }\nactor: { driver: linux-firecracker }\ndefault_executor: missing-executor\n";

fn create_run(home: &Home, profile: &str) -> String {
  let profile = home.write("profile.yaml", profile);
  let output = home.run(&["run", "create", "--profile", profile.to_str().unwrap()]);
  assert!(output.status.success(), "run create: {}", stderr(&output));
  stdout(&output).trim().to_string()
}

// ROOT CAUSE:
//
// If an exec fault named an errno, the command never ran and `needy exec`
// exited 126 with "needy: fault injected: ERRNO", because the errno was only
// a label.
//
// Since 2026-11-12 an exec fault's effect is real: the command exits with
// the fault's code and writes its stderr, as the program would.
#[test]
fn exec_fault_exits_with_its_code_and_writes_its_stderr() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let run = create_run(&home, NATIVE_PROFILE);

  let output = home.run(&["run", "exec", &run, "--", "curl", "http://example.com"]);

  assert_eq!(output.status.code(), Some(7));
  assert_eq!(stderr(&output), "curl: (7) Failed to connect to example.com port 80\n");
}

#[test]
fn chaos_add_ls_and_rm_change_a_live_run_s_faults() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let run = create_run(&home, NATIVE_PROFILE);

  let added = home.run(&[
    "run", "chaos", "add", "crash", "--run", &run, "--exec", "true **", "--signal", "SEGV", "--times", "1",
  ]);
  assert!(added.status.success(), "{}", stderr(&added));
  let crashed = home.run(&["run", "exec", &run, "--", "true"]);
  assert_eq!(crashed.status.code(), Some(128 + 11), "{}", stderr(&crashed));
  // `times: 1`: the next one runs.
  assert_eq!(home.run(&["run", "exec", &run, "--", "true"]).status.code(), Some(0));

  let listed = home.run(&["run", "chaos", "ls", "--run", &run, "-o", "json"]);
  assert!(listed.status.success(), "{}", stderr(&listed));
  let faults: serde_json::Value = serde_json::from_str(&stdout(&listed)).unwrap();
  let rows: Vec<(String, String, u64, u64)> = faults
    .as_array()
    .unwrap()
    .iter()
    .map(|f| {
      (
        f["name"].as_str().unwrap().into(),
        f["source"].as_str().unwrap().into(),
        f["matched"].as_u64().unwrap(),
        f["fired"].as_u64().unwrap(),
      )
    })
    .collect();
  assert_eq!(
    rows,
    [
      ("crash".to_string(), "added".to_string(), 2, 1),
      ("curl-down".to_string(), "profile".to_string(), 0, 0),
      ("slow-sleep".to_string(), "profile".to_string(), 0, 0),
    ]
  );

  // A profile's fault can be removed too, and a removed one is gone.
  assert!(home.run(&["run", "chaos", "rm", "curl-down", "--run", &run]).status.success());
  assert_eq!(home.run(&["run", "exec", &run, "--", "curl", "--version"]).status.code(), Some(0));
  let again = home.run(&["run", "chaos", "rm", "curl-down", "--run", &run]);
  assert!(stderr(&again).contains("has no fault named \"curl-down\""), "{}", stderr(&again));

  // A fault the schema refuses is refused before it reaches the run.
  let refused = home.run(&[
    "run", "chaos", "add", "bad", "--run", &run, "--exec", "curl **", "--errno", "EIO",
  ]);
  assert!(!refused.status.success());
  assert!(stderr(&refused).contains("errno is not for exec faults"), "{}", stderr(&refused));

  // Both changes are in the trace, with who made them.
  let report = home.run(&["run", "report", &run, "-o", "markdown"]);
  assert!(stdout(&report).contains("Chaos changed while running: +crash (exec), -curl-down"), "{}", stdout(&report));
}

#[test]
fn exec_denied_by_a_rule_exits_126() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let run = create_run(&home, NATIVE_PROFILE);

  let output = home.run(&["run", "exec", &run, "--", "rm", "-rf", "/"]);

  assert_eq!(output.status.code(), Some(126));
  assert!(stderr(&output).starts_with("needy: denied: no deleting here [policy.exec_denied]"));
  assert!(stderr(&output).contains("matching routing rule"));
  assert!(!stderr(&output).contains("administrator invitation"));
}

#[test]
fn exec_fault_in_json_output_reports_the_exit_code() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let run = create_run(&home, NATIVE_PROFILE);

  let output = home.run(&[
    "run",
    "exec",
    "--output",
    "json",
    &run,
    "--",
    "curl",
    "http://example.com",
  ]);

  assert_eq!(output.status.code(), Some(7));
  let value: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
  assert_eq!(value["exit_code"], 7);
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
    "run", "exec", "--output", "ndjson", &run, "--", "sleep", "0",
  ]);

  assert_eq!(delayed.status.code(), Some(0), "{}", stderr(&delayed));
  let lines = ndjson(&delayed);
  assert_eq!(types(&lines), ["fault.injected", "exec.decide", "started", "exit"]);
  assert_eq!((&lines[0]["name"], &lines[0]["domain"]), (&serde_json::json!("slow-sleep"), &serde_json::json!("exec")));
  assert_eq!(lines[0]["target"], "sleep 0");
  assert_eq!(lines[0]["effect"]["delay"], 1);
  assert_eq!(lines[1]["argv"], serde_json::json!(["sleep", "0"]));
  assert_eq!(lines[1]["outcome"], serde_json::json!({"run": {"executor": "here"}}));
  assert_eq!(lines[1]["origin"], "cli");

  let refused = home.run(&[
    "run",
    "exec",
    "--output",
    "ndjson",
    &run,
    "--",
    "curl",
    "http://example.com",
  ]);

  assert_eq!(refused.status.code(), Some(7));
  let lines = ndjson(&refused);
  assert_eq!(types(&lines), ["fault.injected", "exec.decide", "stderr", "exit"]);
  assert_eq!(lines[0]["effect"]["exit"], 7);
  assert_eq!(lines[1]["outcome"], serde_json::json!({"fault": "curl-down"}));
  assert_eq!(lines[3]["code"], 7);
}

#[test]
fn exec_ndjson_without_a_run_has_no_decision() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);

  let output = home.run(&["proc", "exec", "--host", "--output", "ndjson", "--", "true"]);

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

  let output = home.run(&[
    "proc",
    "exec",
    "--host",
    "--",
    "sh",
    "-c",
    "echo outside-a-run",
  ]);
  assert_eq!(stdout(&output), "outside-a-run\n");
  assert!(home.run(&["run", "stop", &run]).status.success());

  let report = stdout(&home.run(&[
    "run", "report", &run, "--level", "detailed", "-o", "markdown",
  ]));
  assert!(report.contains("| run.stop |"), "{report}");
  assert!(!report.contains("outside-a-run"), "{report}");
}

#[test]
fn exec_of_a_missing_command_exits_127() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);

  let output = home.run(&[
    "proc",
    "exec",
    "--host",
    "--",
    "/nonexistent/needy-test-command",
  ]);

  assert_eq!(output.status.code(), Some(127));
  let message = stderr(&output);
  assert!(message.starts_with("needy: cannot start /nonexistent/needy-test-command"), "{message}");
  assert_eq!(message.matches("needy:").count(), 1, "{message}");
}

#[test]
fn exec_returns_the_command_exit_code_and_output() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);

  let output = home.run(&[
    "proc",
    "exec",
    "--host",
    "--",
    "sh",
    "-c",
    "echo out; echo err >&2; exit 7",
  ]);

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
  let profile = home.write(
    "profile.yaml",
    "actor: { driver: linux-firecracker }\nexecutors: { here: { driver: native } }\ncommands: [{ match: \"**\", to: hier }]\n",
  );

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

  let output = home.run(&["run", "report", "latest", "-o", "markdown"]);

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

  let output = home.run(&["run", "report", &run, "-o", "markdown"]);

  assert!(output.status.success(), "{}", stderr(&output));
  let title = stdout(&output).lines().next().unwrap().to_string();
  assert_eq!(title, format!("# Run report: {run} (profile: review)"));
}

#[test]
fn run_stop_is_traced_with_who_stopped_the_run() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let run = create_run(&home, NATIVE_PROFILE);
  assert!(home.run(&["run", "stop", &run]).status.success());

  let output = home.run(&[
    "run", "report", &run, "--level", "detailed", "-o", "markdown",
  ]);

  assert!(output.status.success(), "{}", stderr(&output));
  let report = stdout(&output);
  let stop = report.lines().find(|l| l.contains("| run.stop |")).unwrap_or_else(|| panic!("{report}"));
  // The stand-in actor's guest never waits for a stop, so it is killed.
  assert!(stop.contains(r#"| run.stop | `{"by":"operator","actor":{"how":"forced","reason":"#), "{stop}");
  assert!(report.lines().nth(2).is_some_and(|l| !l.contains("incomplete")), "{report}");
}

#[test]
fn run_stop_now_kills_the_actor_and_the_report_says_so() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let run = create_run(&home, NATIVE_PROFILE);
  let both = home.run(&["run", "stop", "--now", "--grace", "1s", &run]);
  assert!(!both.status.success(), "--now and --grace together must be refused");

  let stopped = home.run(&["run", "stop", "--now", &run]);

  assert!(stopped.status.success(), "{}", stderr(&stopped));
  assert!(stderr(&stopped).is_empty(), "a stop asked to be at once warns of nothing: {}", stderr(&stopped));
  let report = stdout(&home.run(&["run", "report", &run]));
  assert!(report.contains("forced: asked to stop at once"), "{report}");
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
  assert_eq!(header, ["ID", "NAME", "STATE", "ACTOR", "GROUP", "EXECUTORS", "EXIT"]);
  // A name and group that default to the ID show as `-`; a running
  // run has no exit yet, and its stand-in actor's guest never boots.
  let row: Vec<&str> = lines.next().unwrap().split_whitespace().collect();
  assert_eq!(row, [run.as_str(), "-", "running", "starting", "-", "here=native"]);
}

/// Runs `needy` with stdout on a pipe whose reader is already gone, as
/// `needy ... | grep -q` leaves it once grep has its match.
fn run_into_a_closed_pipe(home: &Home, args: &[&str]) -> Output {
  let (reader, writer) = std::io::pipe().unwrap();
  drop(reader);
  home.needy().args(args).stdout(writer).stderr(Stdio::piped()).output().unwrap()
}

// ROOT CAUSE:
//
// If stdout's reader went away, as with `needy run ls | grep -q ...`, needy
// panicked with "failed printing to stdout: Broken pipe" because `print!`
// panics on a write error and Rust ignores SIGPIPE.
//
// Before the fix, the command printed a panic message and exited 101.
// The fix keeps every command quiet on a closed stdout: it exits 141
// (128 + SIGPIPE), as a shell reports a program SIGPIPE ended.
#[test]
fn a_closed_stdout_ends_a_command_quietly_with_141() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  create_run(&home, NATIVE_PROFILE);
  let profile = home.write("checked.yaml", NATIVE_PROFILE);

  for args in [
    &["run", "ls"][..],
    &["run", "ls", "--output", "json"],
    &[
      "config",
      "profile",
      "check",
      "--mode",
      "host",
      profile.to_str().unwrap(),
    ],
  ] {
    let output = run_into_a_closed_pipe(&home, args);

    assert_eq!(output.status.code(), Some(141), "{args:?}: {}", stderr(&output));
    assert_eq!(stderr(&output), "", "{args:?}");
  }
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
  assert_eq!(runs[0]["executors"], serde_json::json!([{ "name": "here", "driver": "native", "released": true, "lost": false }]));
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
  assert!(stderr(&output).contains(&format!("run {run} has no actor console")), "{}", stderr(&output));
  assert!(stderr(&output).contains("[run.no_console]"));
}

#[test]
fn profile_check_splits_quoted_commands_and_shows_matching_faults() {
  let home = Home::new();
  let profile = home.write("profile.yaml", NATIVE_PROFILE);

  let output = home.run(&[
    "config",
    "profile",
    "check",
    "--mode",
    "host",
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
  assert!(curl.trim_end().ends_with("curl-down: exit 7, stderr"), "{curl}");
  let ls = text.lines().find(|l| l.starts_with("ls ")).unwrap();
  assert!(!ls.contains("curl-down"), "{ls}");
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
    let output = home.run(&[
      "config",
      "profile",
      "check",
      "--mode",
      "host",
      profile.to_str().unwrap(),
    ]);

    assert!(output.status.success(), "{}: {}{}", profile.display(), stdout(&output), stderr(&output));
    assert!(!stdout(&output).contains("error:"), "{}: {}", profile.display(), stdout(&output));
  }
}

#[test]
fn profile_check_reports_unknown_names_and_exits_1() {
  let home = Home::new();
  let profile = home.write(
    "profile.yaml",
    "actor: { driver: linux-firecracker }\nexecutors: { build: {} }\ncommands:\n  - { match: \"git **\", to: biuld }\n  - { match: \"make **\", use: [fast] }\n",
  );

  let output = home.run(&[
    "config",
    "profile",
    "check",
    "--mode",
    "host",
    profile.to_str().unwrap(),
  ]);

  assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
  let text = stdout(&output);
  assert!(text.contains("error: commands[0] (git **) sends to unknown executor \"biuld\"\n"), "{text}");
  assert!(text.contains("error: commands[1] (make **) uses unknown preset \"fast\"\n"), "{text}");
}

// A rule without `to:` must still reach the daemon from an actor, so
// `profile check` lists its command among the route names.
#[test]
fn profile_check_lists_route_names_of_rules_without_a_placement() {
  let home = Home::new();
  let profile = home.write(
    "profile.yaml",
    "actor: { driver: linux-firecracker }\ncommands:\n  - { match: \"rm -rf **\", deny: \"not here\" }\n  - { match: \"npm **\", env: { set: { CI: \"1\" } } }\n",
  );

  let output = home.run(&[
    "config",
    "profile",
    "check",
    "--mode",
    "host",
    profile.to_str().unwrap(),
  ]);

  let text = stdout(&output);
  let line = text.lines().find(|l| l.starts_with("route names: ")).unwrap_or_else(|| panic!("{text}{}", stderr(&output)));
  let names: Vec<&str> = line["route names: ".len()..].split(',').collect();
  assert!(names.contains(&"rm") && names.contains(&"npm") && names.contains(&"sh"), "{line}");
}

// An exec fault must reach the daemon from an actor even when no rule names
// its command, so `profile check` lists the fault's command too.
#[test]
fn profile_check_lists_route_names_of_exec_faults() {
  let home = Home::new();
  let profile =
    home.write("profile.yaml", "actor: { driver: linux-firecracker }\nchaos: { faults: { curl-down: { exec: \"curl **\", exit: 7 } } }\n");

  let output = home.run(&[
    "config",
    "profile",
    "check",
    "--mode",
    "host",
    profile.to_str().unwrap(),
  ]);

  let text = stdout(&output);
  let line = text.lines().find(|l| l.starts_with("route names: ")).unwrap_or_else(|| panic!("{text}{}", stderr(&output)));
  let names: Vec<&str> = line["route names: ".len()..].split(',').collect();
  assert!(names.contains(&"curl") && names.contains(&"sh"), "{line}");
}

// Captures used to sit directly under `when`, where a misspelt condition
// became a capture that never matched. They now go under `captures:`.
#[test]
fn profile_check_rejects_flattened_captures() {
  let home = Home::new();
  let profile = home.write(
    "profile.yaml",
    "actor: { driver: linux-firecracker }\ncommands: [{ match: \"node {script}\", when: { script: \"**/cli.js\" }, to: actor }]\n",
  );

  let output = home.run(&["config", "profile", "check", profile.to_str().unwrap()]);

  assert_eq!(output.status.code(), Some(1));
  let message = stderr(&output);
  assert!(message.starts_with(&format!("needy: invalid profile {}: ", profile.display())), "{message}");
  assert!(message.contains("commands[0].when.script: unknown field `script`, expected one of"), "{message}");
}

#[test]
fn profile_check_compiles_a_built_in_recipe() {
  let home = Home::new();
  let profile = home.write("profile.yaml", "actor: { driver: linux-firecracker, recipe: \"builtin:gce-ubuntu-22.04\" }\n");

  let output = home.run(&[
    "config",
    "profile",
    "check",
    "--mode",
    "host",
    profile.to_str().unwrap(),
  ]);

  // The host CPU decides whether the recipe has findings, so only the
  // compiled machine is checked here.
  let text = stdout(&output);
  assert!(text.contains("recipe: 2 vCPU, 8192 MiB"), "{text}{}", stderr(&output));
  assert!(text.contains("  smbios: sys_vendor=Google, product_name=Google Compute Engine,"), "{text}");
}

// ROOT CAUSE:
//
// If a recipe's hostname or SMBIOS serial and UUID came from the run's
// seed, `profile check` printed them compiled with a fixed seed of 1, and
// the hostname with Debug formatting: `hostname Some("instance-…")`. A run
// compiles the recipe with a seed from its ID, so it got other values.
//
// Before the fix, the check showed one fixed value the run never had.
// The fix prints a value that depends on the seed as generated per run.
#[test]
fn profile_check_says_which_recipe_values_each_run_generates() {
  let home = Home::new();
  let profile = home.write("profile.yaml", "actor: { driver: linux-firecracker, recipe: \"builtin:gce-ubuntu-22.04\" }\n");

  let output = home.run(&[
    "config",
    "profile",
    "check",
    "--mode",
    "host",
    profile.to_str().unwrap(),
  ]);

  let text = stdout(&output);
  assert!(text.contains("recipe: 2 vCPU, 8192 MiB, hostname generated per run, uptime offset"), "{text}{}", stderr(&output));
  assert!(!text.contains("Some("), "{text}");
  assert!(text.contains("serial=generated per run"), "{text}");
  assert!(text.contains("uuid=generated per run"), "{text}");
  assert!(text.contains("  cpu brand: Intel(R) Xeon(R) CPU @ 2.80GHz\n"), "{text}");
}

// `{time}` is the run's creation time, which a check does not have either.
#[test]
fn profile_check_says_a_hostname_from_the_runs_time_is_generated_per_run() {
  let home = Home::new();
  home.write("time.recipe.yaml", "os: { hostname: \"ci-{time}\" }\n");
  let profile = home.write("profile.yaml", "actor: { driver: linux-firecracker, recipe: time.recipe.yaml }\n");

  let output = home.run(&[
    "config",
    "profile",
    "check",
    "--mode",
    "host",
    profile.to_str().unwrap(),
  ]);

  let text = stdout(&output);
  assert!(text.contains(", hostname generated per run, "), "{text}{}", stderr(&output));
}

#[test]
fn profile_check_asks_the_control_plane_where_the_profile_would_run() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let profile = home.write("profile.yaml", NATIVE_PROFILE);

  let output = home.run(&[
    "config",
    "profile",
    "check",
    "--mode",
    "control",
    profile.to_str().unwrap(),
    "--cmd",
    "ls",
  ]);

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
  let profile =
    home.write("profile.yaml", "executors: { mac: { runner: mac } }\nactor: { driver: linux-firecracker, runner: { labels: [kvm] } }\n");

  let output = home.run(&[
    "config",
    "profile",
    "check",
    "--mode",
    "control",
    profile.to_str().unwrap(),
  ]);

  assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
  let text = stdout(&output);
  assert!(text.contains("\nactor: unresolved\n"), "{text}");
  assert!(text.contains("\nexecutors: mac (unresolved)\n"), "{text}");
  assert!(text.contains("error: actor: no connected runner with actor(linux-firecracker) has labels kvm; no runner has joined\n"), "{text}");
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
  assert!(text.contains("--profile <PROFILE>") && text.contains("Profile YAML"), "{text}");
  assert!(text.contains("--group <GROUP>") && text.contains("Runs in one group share executors"), "{text}");
  assert!(text.contains("Defaults to the run ID"), "{text}");
}

#[test]
fn run_create_on_a_terminal_can_be_attached_later() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let profile = home.write("profile.yaml", NATIVE_PROFILE);

  let created = home.run(&[
    "run",
    "create",
    "-t",
    "--profile",
    profile.to_str().unwrap(),
  ]);
  assert!(created.status.success(), "{}", stderr(&created));
  let id = stdout(&created).trim().to_string();
  let listed = home.run(&["run", "ls", "-o", "json"]);
  let rows: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
  let run = rows.as_array().unwrap().iter().find(|r| r["id"] == id).unwrap();
  assert_eq!(run["tty"], true);
}

#[test]
fn run_attach_explains_when_the_driving_program_has_no_terminal() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let run = create_run(&home, NATIVE_PROFILE);

  let output = home.run(&["run", "attach", &run]);

  assert_eq!(output.status.code(), Some(1));
  assert!(stderr(&output).contains("has no terminal; create the run with `needy run create -t`"), "{}", stderr(&output));
}

// ROOT CAUSE:
//
// If `run create` got `-i` without `-t`, clap's generic `requires` error
// named the missing argument but not why `-i` needs it.
//
// Before the fix, the message was clap's usage error.
// The fix says what `-i` does and that it needs `-t`, still as a usage error.
#[test]
fn run_create_interactive_requires_a_terminal() {
  let home = Home::new();
  let output = home.run(&["run", "create", "-i", "--profile", "example.yaml"]);
  assert_eq!(output.status.code(), Some(2));
  assert_eq!(stderr(&output), "needy: `-i` attaches your terminal to the driving program, which needs one: use `-it`\n");
}

// ROOT CAUSE:
//
// A fast driving program could exit and reboot its VM before `-it` attached.
// The run recorded its result, but the client waited for a terminal that
// no longer existed. An interactive create now subscribes before starting
// the program and drains its output and exit before rebooting the guest.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires usable KVM, NEEDY_TEST_ACTOR_IMAGE built from this tree and NEEDY_TEST_GUEST_NEEDY (`make guest`)"]
fn an_immediately_exiting_interactive_actor_returns_its_output_and_exit() {
  let image = std::env::var_os("NEEDY_TEST_ACTOR_IMAGE").expect("set NEEDY_TEST_ACTOR_IMAGE to a freshly built actor image");
  let guest = std::env::var_os("NEEDY_TEST_GUEST_NEEDY").expect("set NEEDY_TEST_GUEST_NEEDY to the guest needy `make guest` builds");
  let home = Home::new();
  let child = home
    .needy()
    .args(["daemon", "serve", "--actor-image"])
    .arg(image)
    .arg("--guest-needy")
    .arg(guest)
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .unwrap();
  let _daemon = Daemon { child };
  let socket = home.path().join("run/daemon.sock");
  let deadline = Instant::now() + Duration::from_secs(10);
  while std::os::unix::net::UnixStream::connect(&socket).is_err() {
    assert!(Instant::now() < deadline, "the daemon did not listen");
    std::thread::sleep(Duration::from_millis(20));
  }
  let profile = home.write("profile.yaml", "user: { name: agent, uid: 1000, gid: 1000 }\nactor: { driver: linux-firecracker }\n");
  let mut client = home
    .needy()
    .args(["run", "create", "-it", "--profile"])
    .arg(profile)
    .args(["--", "sh", "-c", "printf 'short-run\\n'; exit 9"])
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
  let deadline = Instant::now() + Duration::from_secs(15);
  let mut timed_out = false;
  while client.try_wait().unwrap().is_none() {
    if Instant::now() >= deadline {
      client.kill().unwrap();
      timed_out = true;
      break;
    }
    std::thread::sleep(Duration::from_millis(20));
  }
  let output = client.wait_with_output().unwrap();
  // A real VM does not share the stand-in's parent-liveness loop. Stop
  // every run in this isolated test home before reporting a failed check.
  let listed = home.run(&["run", "ls", "-o", "json"]);
  let runs: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
  for run in runs.as_array().unwrap() {
    let stopped = home.run(&["run", "stop", run["id"].as_str().unwrap()]);
    assert!(stopped.status.success(), "{}", stderr(&stopped));
  }
  assert!(!timed_out, "the fast actor did not return: {}", stderr(&output));
  assert_eq!(output.status.code(), Some(9), "{}", stderr(&output));
  assert!(stdout(&output).contains("short-run"), "{}", stdout(&output));
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

  let output = home.run(&["run", "report", "--offline"]);

  assert_eq!(output.status.code(), Some(1));
  let expected = home.path().join("state").join("trace.db");
  assert!(stderr(&output).contains(expected.to_str().unwrap()), "{}", stderr(&output));
}

#[test]
fn state_directory_defaults_to_xdg_state_home() {
  let home = Home::new();

  let output = home
    .needy()
    .env_remove("NEEDY_STATE_DIR")
    .env("XDG_STATE_HOME", home.path().join("xdg"))
    .args(["run", "report", "--offline"])
    .output()
    .unwrap();

  let expected = home.path().join("xdg").join("needy").join("trace.db");
  assert!(stderr(&output).contains(expected.to_str().unwrap()), "{}", stderr(&output));
}

#[test]
fn runtime_defaults_to_the_state_directory_without_a_runtime_directory() {
  let home = Home::new();
  let output = home.needy().env_remove("NEEDY_RUNTIME_DIR").args(["config", "paths", "-o", "json"]).output().unwrap();
  assert!(output.status.success(), "{}", stderr(&output));
  let rows: Vec<serde_json::Value> = serde_json::from_str(&stdout(&output)).unwrap();
  let runtime = rows.iter().find(|row| row["name"] == "runtime").unwrap();
  assert_eq!(runtime["source"], home.path().join("state").display().to_string());
}

/// Sends signal `name` to process `pid`.
fn kill(pid: u32, name: &str) {
  let status = Command::new("kill").args([&format!("-{name}"), &pid.to_string()]).status().unwrap();
  assert!(status.success(), "kill -{name} {pid}");
}

/// A foreground control or standalone executor on this home's socket.
fn spawn_daemon(home: &Home, mode: &str) -> Child {
  let mut command = home.needy();
  let socket = home.path().join("run").join("daemon.sock");
  if mode == "control" {
    command.args(["daemon", "serve"]).args(stand_in_actor(home));
  } else {
    command.args([
      "daemon",
      "executor",
      "--listen",
      &format!("unix:{}", socket.display()),
    ]);
  }
  let daemon = command.stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
  let deadline = Instant::now() + Duration::from_secs(20);
  while std::os::unix::net::UnixStream::connect(&socket).is_err() || (mode == "control" && !home.path().join("run/control.json").exists()) {
    assert!(Instant::now() < deadline, "the daemon did not listen on {}", socket.display());
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
  let (attached, sleep) = attach_sleep(&home, &["run", "exec", &run]);

  assert!(home.run(&["run", "stop", &run]).status.success());
  let output = exited(attached);
  // Leaving the stream ends the sleep; this is for a failed assertion only.
  let _ = Command::new("kill").args(["-KILL", &sleep.to_string()]).status();

  assert_eq!(output.status.code(), Some(255), "{}", stderr(&output));
  assert!(
    stderr(&output).starts_with("needy: the connection ended before the command exited: run stopped [process.outcome_unknown]"),
    "{}",
    stderr(&output)
  );
}

#[test]
fn exec_json_whose_run_stops_reports_an_error_instead_of_an_exit() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let run = create_run(&home, NATIVE_PROFILE);
  let (attached, _) = attach_sleep(&home, &["run", "exec", "--output", "json", &run]);

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
  let (attached, sleep) = attach_sleep(&home, &["proc", "exec", "--host"]);

  kill(daemon.id(), "TERM");
  let output = exited(attached);
  daemon.wait().unwrap();
  let _ = Command::new("kill").args(["-KILL", &sleep.to_string()]).status();

  assert_eq!(output.status.code(), Some(255), "{}", stderr(&output));
  assert!(
    stderr(&output).starts_with("needy: the connection ended before the command exited: daemon shutting down [process.outcome_unknown]"),
    "{}",
    stderr(&output)
  );
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
            diagnostic: None,
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
    "--address",
    &daemon.endpoint,
    "proc",
    "exec",
    "--host",
    "-d",
    "--",
    "sleep",
    "30",
  ]);

  assert_eq!(output.status.code(), Some(255), "{}", stderr(&output));
  assert_eq!(stdout(&output), "");
  assert!(
    stderr(&output).starts_with("needy: the connection ended before the command started: run stopped [process.outcome_unknown]"),
    "{}",
    stderr(&output)
  );
}

#[test]
fn detached_exec_json_whose_stream_closes_before_the_start_reports_an_error() {
  let home = Home::new();
  let daemon = EndingDaemon::start(&home, None);

  let output = home.run(&[
    "--address",
    &daemon.endpoint,
    "proc",
    "exec",
    "--host",
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

// ROOT CAUSE:
//
// Early stream completion returned 255 through the exec-specific output path,
// bypassing global diagnostic rendering. A caller could not distinguish an
// unknown outcome from a completed child that deliberately exited 255.
#[test]
fn an_unknown_exec_outcome_honors_json_diagnostics_without_changing_stdout() {
  let home = Home::new();
  let daemon = EndingDaemon::start(&home, None);
  let output = home.run(&[
    "--diagnostic-format",
    "json",
    "--address",
    &daemon.endpoint,
    "proc",
    "exec",
    "--host",
    "-d",
    "--output",
    "json",
    "--",
    "sleep",
    "30",
  ]);
  assert_eq!(output.status.code(), Some(255));
  let summary: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  assert_eq!(summary["error"], "the connection ended before the command started");
  assert!(summary.get("exit_code").is_none());
  let diagnostic: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
  assert_eq!(diagnostic["reason"], "process.outcome_unknown");
  assert_eq!(diagnostic["completion"], "unknown");
  assert_eq!(diagnostic["checks"][0]["location"], "client");
  let argv = diagnostic["checks"][0]["argv"].as_array().unwrap();
  assert_eq!(argv[0], "needy");
  assert!(argv.windows(2).any(|args| args[0] == "--address" && args[1] == daemon.endpoint));
  assert_eq!(&argv[argv.len() - 2..], &[serde_json::json!("proc"), serde_json::json!("ls")]);
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
  // An actor whose VM ignores SIGTERM keeps the run's stop, and so the
  // graceful shutdown, busy for the VM's stop grace period.
  let firecracker = home.path().join("image").join("firecracker");
  needy_common::testing::write_executable(
    &firecracker,
    "#!/bin/sh\ntrap '' TERM\ntouch \"$(dirname \"$3\")/trapped\"\nwhile kill -0 $PPID 2>/dev/null; do sleep 0.05; done\n",
  )
  .unwrap();
  let run = create_run(&home, NATIVE_PROFILE);
  let (mut attached, sleep) = attach_sleep(&home, &["run", "exec", &run]);

  // The VM ignores SIGTERM once it says so.
  let trapped = home.path().join("state").join("runs").join(&run).join("trapped");
  let deadline = Instant::now() + Duration::from_secs(20);
  while !trapped.exists() {
    assert!(Instant::now() < deadline, "the stand-in VM did not start");
    std::thread::sleep(Duration::from_millis(20));
  }
  let (elapsed, output) = terminate_twice(daemon);
  attached.kill().ok();
  attached.wait().unwrap();
  // A forced exit may leave the sleep behind. The run's stop, which closed
  // the sleep's stream to the executor, may have hung it up already, so the
  // status of a `kill` that finds no process is irrelevant.
  let _ = Command::new("kill").args(["-KILL", &sleep.to_string()]).stderr(Stdio::null()).status();

  assert!(elapsed < Duration::from_secs(2), "the daemon took {elapsed:?} to exit after the second SIGTERM");
  assert_eq!(output.status.code(), Some(1));
  assert!(stderr(&output).contains(&run), "the exit names the run it left: {}", stderr(&output));
  // Even a forced exit removes the daemon's own sockets.
  assert!(!home.path().join("run").join("daemon.sock").exists(), "daemon.sock is left behind");
  assert!(!home.path().join("run").join("local.sock").exists(), "local.sock is left behind");
}

#[test]
fn a_second_sigterm_makes_the_executor_exit_at_once() {
  let home = Home::new();
  let daemon = spawn_daemon(&home, "executor");
  let address = format!("unix:{}", home.path().join("run/daemon.sock").display());
  let (mut attached, sleep) = attach_sleep(&home, &["--address", &address, "proc", "exec", "--host"]);

  let (elapsed, output) = terminate_twice(daemon);
  attached.kill().ok();
  attached.wait().unwrap();
  // A forced exit leaves the sleep behind.
  kill(sleep, "KILL");

  assert!(elapsed < Duration::from_secs(2), "the executor took {elapsed:?} to exit after the second SIGTERM");
  assert_eq!(output.status.code(), Some(1));
  assert!(stderr(&output).contains("these processes were not killed"), "{}", stderr(&output));
  assert!(!home.path().join("run").join("daemon.sock").exists(), "daemon.sock is left behind");
}

/// Waits for `daemon` to exit by itself and returns its output.
fn daemon_exit(mut daemon: Child) -> Output {
  let deadline = Instant::now() + Duration::from_secs(20);
  while daemon.try_wait().unwrap().is_none() {
    if Instant::now() >= deadline {
      daemon.kill().ok();
      panic!("the daemon did not exit: {:?}", daemon.wait_with_output().unwrap());
    }
    std::thread::sleep(Duration::from_millis(20));
  }
  daemon.wait_with_output().unwrap()
}

// ROOT CAUSE:
//
// If a daemon exited, even gracefully at SIGTERM, its Unix sockets stayed
// in the runtime directory, because nothing removed a socket file once its
// listener closed; only the next start replaced it.
//
// Before the fix, `daemon.sock` and `local.sock` outlived the daemon.
// The fix removes the sockets a server bound when it stops serving.
#[test]
fn a_control_daemon_removes_its_sockets_when_it_exits_at_sigterm() {
  let home = Home::new();
  let daemon = spawn_daemon(&home, "control");
  let run = home.path().join("run");
  assert!(run.join("local.sock").exists());

  kill(daemon.id(), "TERM");
  let output = daemon_exit(daemon);

  assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
  assert!(!run.join("daemon.sock").exists(), "daemon.sock is left behind");
  assert!(!run.join("local.sock").exists(), "local.sock is left behind");
}

#[test]
fn an_executor_daemon_removes_its_socket_when_it_exits_at_sigterm() {
  let home = Home::new();
  let daemon = spawn_daemon(&home, "executor");

  kill(daemon.id(), "TERM");
  let output = daemon_exit(daemon);

  assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
  assert!(!home.path().join("run").join("daemon.sock").exists(), "daemon.sock is left behind");
}

#[test]
fn a_daemon_starts_over_stale_sockets_nobody_listens_on() {
  let home = Home::new();
  let run = home.path().join("run");
  std::fs::create_dir_all(&run).unwrap();
  {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700)).unwrap();
  }
  // A daemon that was killed leaves its sockets bound to no listener.
  for name in ["daemon.sock", "local.sock"] {
    drop(std::os::unix::net::UnixListener::bind(run.join(name)).unwrap());
  }

  let _daemon = Daemon::start(&home);

  // NOTICE: this test failed once under a full-suite load on the Linux KVM
  // host (2026-11-12) with no output to say why; 2,000 runs under load and
  // 48 full suites did not fail again (2026-11-13). `ps`'s stderr names the
  // cause if it does.
  let ps = home.run(&["proc", "ls"]);
  assert!(ps.status.success(), "ps: {}", stderr(&ps));
}

// ROOT CAUSE:
//
// If a daemon started while another listened on the same socket, it took
// the socket over, because binding removed whatever file was at the path
// without asking whether a server still answered there.
//
// Before the fix, the second daemon unlinked the first one's sockets and
// served in its place, and the first one served on nothing reachable.
// The fix refuses to start, naming the socket another server listens on.
#[test]
fn a_second_daemon_on_the_same_sockets_is_refused() {
  use std::os::unix::fs::MetadataExt;
  let home = Home::new();
  let _first = Daemon::start(&home);
  let socket = home.path().join("run").join("daemon.sock");
  let first_socket = std::fs::metadata(&socket).unwrap().ino();

  let second =
    home.needy().args(["daemon", "serve"]).args(stand_in_actor(&home)).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
  let output = daemon_exit(second);

  assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
  assert!(stderr(&output).contains("[discovery.instance_conflict]"), "{}", stderr(&output));
  assert_eq!(std::fs::metadata(&socket).unwrap().ino(), first_socket, "the first daemon's socket was replaced");
  assert!(home.run(&["proc", "ls"]).status.success(), "the first daemon no longer answers");
}

/// The table rows `needy ps` prints, split into cells by two or more spaces.
fn table(text: &str) -> Vec<Vec<String>> {
  text.lines().map(|line| line.split("  ").map(str::trim).filter(|c| !c.is_empty()).map(str::to_string).collect()).collect()
}

/// A native run with one `needy exec` and one `needy fs cp`, whose
/// `mkdir` and `tar` are needy's own processes.
fn run_with_internal_processes(home: &Home) -> String {
  let run = create_run(home, NATIVE_PROFILE);
  let exec = home.run(&["run", "exec", &run, "--", "echo", "mine"]);
  assert!(exec.status.success(), "{}", stderr(&exec));
  let file = home.write("copied.txt", "copied\n");
  let dir = home.path().join("into");
  let cp = home.run(&[
    "fs",
    "cp",
    "--run",
    &run,
    "--executor",
    "here",
    file.to_str().unwrap(),
    &format!("remote:{}", dir.display()),
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

  let output = home.run(&["proc", "ls", "--run", &run]);

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

  let output = home.run(&["proc", "ls", "-a", "--run", &run]);

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

  let mine = json(&["proc", "ls", "-o", "json", "--run", &run]);
  let all = json(&["proc", "ls", "-o", "json", "--all", "--run", &run]);

  assert_eq!(mine.len(), 1, "{mine:?}");
  assert_eq!((mine[0]["run"].as_str(), mine[0]["origin"].as_str()), (Some(run.as_str()), Some("cli")));
  let origins: Vec<&str> = all.iter().map(|p| p["origin"].as_str().unwrap()).collect();
  assert_eq!(origins, ["cli", "fs", "fs"]);
}

/// Starts `command` detached in `run` and returns its process ID.
fn detach(home: &Home, run: &str, command: &[&str]) -> String {
  let output = home.needy().args(["run", "exec", "-d", run, "--"]).args(command).output().unwrap();
  assert!(output.status.success(), "{}", stderr(&output));
  stdout(&output).trim().to_string()
}

/// Waits until `needy ps` no longer lists process `id` as running.
fn wait_until_ended(home: &Home, run: &str, id: &str) {
  let deadline = Instant::now() + Duration::from_secs(20);
  loop {
    let listed: Vec<serde_json::Value> = serde_json::from_str(&stdout(&home.run(&["proc", "ls", "-o", "json", "--run", run]))).unwrap();
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
  assert!(home.run(&["proc", "kill", &killed, "--signal", "SIGINT"]).status.success());
  wait_until_ended(&home, &run, &exited);
  wait_until_ended(&home, &run, &killed);

  let output = home.run(&["proc", "ls", "--run", &run]);

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
  assert!(home.run(&["proc", "kill", &running, "--signal", "SIGKILL"]).status.success());
}

#[test]
fn ps_json_has_separate_state_exit_code_and_signal() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let run = create_run(&home, NATIVE_PROFILE);
  let exited = detach(&home, &run, &["sh", "-c", "exit 3"]);
  let killed = detach(&home, &run, &["sleep", "31"]);
  assert!(home.run(&["proc", "kill", &killed, "--signal", "SIGINT"]).status.success());
  wait_until_ended(&home, &run, &exited);
  wait_until_ended(&home, &run, &killed);

  let output = home.run(&["proc", "ls", "-o", "json", "--run", &run]);

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
  let listed: Vec<serde_json::Value> = serde_json::from_str(&stdout(&home.run(&["proc", "ls", "-a", "-o", "json", "--run", &run]))).unwrap();
  let tar = listed.iter().find(|p| p["argv"][0] == "tar").unwrap()["id"].as_str().unwrap().to_string();

  let output = home.run(&["proc", "attach", &tar]);

  assert_eq!(output.status.code(), Some(1));
  assert!(stderr(&output).contains(&format!("process {tar} is needy\'s own (origin fs)")), "{}", stderr(&output));
  assert!(stderr(&output).contains("[process.internal_attach_denied]"));
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
  assert!(home.run(&["run", "exec", &run, "--", "sh", "-c", "exit 2"]).status.code() == Some(2));
  assert!(home.run(&["run", "stop", &run]).status.success());

  let output = home.run(&["run", "report", &run]);

  assert!(output.status.success(), "{}", stderr(&output));
  let report = stdout(&output);
  let mut lines = report.lines();
  assert_eq!(lines.next(), Some(format!("Run         {run}  (profile review)").as_str()));
  assert_eq!(lines.next(), Some("State       stopped"));
  assert!(lines.next().is_some_and(|l| l.starts_with("Duration    ") && l.ends_with('s')), "{report}");
  assert_eq!(lines.next(), Some("Actor       linux-firecracker"));
  assert_eq!(lines.next(), Some("Started by  operator"));
  assert_eq!(lines.next(), Some("Stopped by  operator"));
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
    "run",
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

// A standalone executor uses an explicit incoming private credential on every
// network listener, including loopback. Reachability alone is not admission.
#[test]
fn a_network_executor_requires_an_explicit_private_token_file() {
  let home = Home::new();
  for address in ["tcp://0.0.0.0:0", "tcp://127.0.0.1:0"] {
    let (exited, output) = daemon_within(&home, &["executor", "--listen", address], Duration::from_secs(10));
    assert!(exited);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(stderr(&output).contains("--token-file"), "{}", stderr(&output));
  }
}

#[test]
fn an_executor_refuses_an_insecure_private_token_file() {
  let home = Home::new();
  use std::os::unix::fs::PermissionsExt as _;
  let token = home.write("incoming-token", "synthetic-token");
  std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o644)).unwrap();
  let (exited, output) = daemon_within(
    &home,
    &[
      "executor",
      "--listen",
      "tcp://127.0.0.1:0",
      "--token-file",
      token.to_str().unwrap(),
    ],
    Duration::from_secs(10),
  );
  assert!(exited);
  assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
  assert!(stderr(&output).contains("private"), "{}", stderr(&output));
  assert!(!stderr(&output).contains("synthetic-token"));
}

#[test]
fn an_executor_refuses_a_missing_private_token_file() {
  let home = Home::new();
  let missing = home.path().join("missing-token");
  let (exited, output) = daemon_within(
    &home,
    &[
      "executor",
      "--listen",
      "tcp://127.0.0.1:0",
      "--token-file",
      missing.to_str().unwrap(),
    ],
    Duration::from_secs(10),
  );
  assert!(exited);
  assert_eq!(output.status.code(), Some(1));
  assert!(stderr(&output).contains("credential file"), "{}", stderr(&output));
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
    "umask 000; exec \"$0\" daemon serve \"$@\"",
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
  // Wait for discovery publication as well as socket admission.
  while std::os::unix::net::UnixStream::connect(&socket).is_err() {
    assert!(Instant::now() < deadline, "the daemon did not listen on {}", socket.display());
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
    .args(["daemon", "serve"])
    .args(stand_in_actor(&home))
    .current_dir(&elsewhere)
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .unwrap();
  let socket = home.path().join("run").join("daemon.sock");
  let deadline = Instant::now() + Duration::from_secs(20);
  // Wait for discovery publication as well as socket admission.
  while std::os::unix::net::UnixStream::connect(&socket).is_err() {
    assert!(Instant::now() < deadline, "the daemon did not listen on {}", socket.display());
    std::thread::sleep(Duration::from_millis(50));
  }
  let _daemon = Daemon { child };
  std::fs::create_dir_all(home.path().join("p")).unwrap();
  home.write("p/base.yaml", NATIVE_PROFILE);
  home.write("p/gce.recipe.yaml", "extends: builtin:gce-ubuntu-22.04\n");
  let profile = home.write("p/profile.yaml", "extends: base.yaml\nactor: { driver: linux-firecracker, recipe: gce.recipe.yaml }\n");

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

  let (code, written) = on_terminal(&home, &["proc", "exec", "--host", "-t", "--", "sh", "-c", modes], (24, 80));

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

  let (code, written) = on_terminal_with(
    &home,
    &[
      "proc",
      "exec",
      "--host",
      "-t",
      "--",
      "sh",
      "-c",
      ignores_term,
    ],
    (24, 80),
    |pid, written| {
      let deadline = Instant::now() + Duration::from_secs(20);
      while !written.lock().unwrap().windows(8).any(|w| w == b"MODES-ON") {
        assert!(Instant::now() < deadline, "the program did not start");
        std::thread::sleep(Duration::from_millis(20));
      }
      kill(pid, "TERM");
      std::thread::sleep(Duration::from_millis(300));
      kill(pid, "TERM");
    },
  );

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
      "proc",
      "exec",
      "--host",
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
  let output = home.needy().args(["runner", "invite", "-o", "json"]).args(args).output().unwrap();
  assert!(output.status.success(), "token create: {}", stderr(&output));
  let issued: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
  issued["code"].as_str().unwrap().to_owned()
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
  assert!(stderr(&missing).contains("no join token nope") && stderr(&missing).contains("[bootstrap.invalid_code]"), "{}", stderr(&missing));
}

#[test]
fn runner_token_create_refuses_an_unknown_role_and_a_malformed_label() {
  let home = Home::new();

  let role = home.run(&["runner", "invite", "--roles", "builder"]);
  let label = home.run(&["runner", "invite", "--labels", "os"]);

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
  assert_eq!(
    stdout(&output).split_whitespace().collect::<Vec<_>>(),
    [
      "NAME",
      "STATUS",
      "CAPABILITIES",
      "HOST",
      "CPU",
      "LABELS",
      "LAST",
      "SEEN"
    ]
  );
}

#[test]
fn runner_rotate_and_rm_name_a_missing_runner() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);

  let rotate = home.run(&["runner", "rotate", "ghost"]);
  let rm = home.run(&["runner", "rm", "ghost"]);

  for output in [&rotate, &rm] {
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(output).contains("no runner ghost") && stderr(output).contains("[runner.not_found]"), "{}", stderr(output));
  }
}

/// A control daemon whose runners send a heartbeat every 200 ms.
fn spawn_control_with_fast_heartbeats(home: &Home) -> Daemon {
  let child = home
    .needy()
    .args(["daemon", "serve", "--heartbeat-interval", "200ms"])
    .args(stand_in_actor(home))
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .unwrap();
  let socket = home.path().join("run").join("daemon.sock");
  let deadline = Instant::now() + Duration::from_secs(20);
  // Wait for discovery publication as well as socket admission.
  while std::os::unix::net::UnixStream::connect(&socket).is_err() || !home.path().join("run/control.json").exists() {
    assert!(Instant::now() < deadline, "the daemon did not listen on {}", socket.display());
    std::thread::sleep(Duration::from_millis(50));
  }
  Daemon { child }
}

/// Enroll once, then start a runner from its independent saved credential.
fn enroll_runner(home: &Home, control: &Home, token: &str, extra: &[String]) -> Output {
  let endpoint = format!("unix:{}", control.path().join("run/daemon.sock").display());
  let invitation = home.write("invitation", token);
  {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&invitation, std::fs::Permissions::from_mode(0o600)).unwrap();
  }
  home
    .needy()
    .args([
      "--address",
      &endpoint,
      "runner",
      "join",
      "mac",
      "--code-file",
      invitation.to_str().unwrap(),
      "--labels",
      "os=test",
    ])
    .args(extra)
    .env("DOCKER_HOST", format!("unix://{}", home.path().join("no-docker.sock").display()))
    .env("PATH", path_without_apple_container())
    .output()
    .unwrap()
}

fn spawn_runner(home: &Home, control: &Home, join_token: Option<&str>) -> Child {
  if let Some(token) = join_token {
    let joined = enroll_runner(home, control, token, &[]);
    assert!(joined.status.success(), "runner join: {}", stderr(&joined));
  }
  home
    .needy()
    .args(["runner", "serve"])
    .env("DOCKER_HOST", format!("unix://{}", home.path().join("no-docker.sock").display()))
    .env("PATH", path_without_apple_container())
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap()
}

/// This test's `PATH` without the directories that hold Apple's `container`
/// CLI, so a runner detects the same capabilities on a Mac with it as on a
/// machine without it, as `DOCKER_HOST` does for Docker.
fn path_without_apple_container() -> std::ffi::OsString {
  let path = std::env::var_os("PATH").unwrap_or_default();
  std::env::join_paths(std::env::split_paths(&path).filter(|dir| !dir.join("container").is_file())).unwrap()
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
  // Read after the first runner exited, which waits for a heartbeat in
  // flight: a heartbeat it sent after `joined` is not the restart.
  let left = runner_when(&control, "mac", |_| true);
  let second = spawn_runner(&machine, &control, None);
  // Online as a listing reads it: a slow listing can read a fresh
  // heartbeat after more than the three intervals that keep it online.
  let restarted =
    runner_when(&control, "mac", |r| r["status"] == "online" && r["last_seen_at_ms"].as_i64() > left["last_seen_at_ms"].as_i64());
  let reported = runner_when(&control, "mac", |r| !r["host"].is_null());
  let rm = control.run(&["runner", "rm", "mac"]);
  let refused = exited(second);

  assert_eq!(joined["labels"]["os"], "test");
  assert_eq!(joined["capabilities"], serde_json::json!({"actor": [], "executor": ["native"]}));
  assert_eq!(restarted["labels"]["os"], "test");
  assert_eq!(reported["host"]["os"], std::env::consts::OS);
  assert_eq!(reported["host"]["arch"], std::env::consts::ARCH);
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

/// A control plane whose `Join` answers only once the test releases it,
/// and which refuses everything else. It stops when dropped.
struct HeldJoin {
  endpoint: String,
  arrived: std::sync::mpsc::Receiver<()>,
  release: std::sync::Arc<tokio::sync::Notify>,
  _runtime: tokio::runtime::Runtime,
}

impl HeldJoin {
  const CREDENTIAL: &str = "held-join-credential";

  fn start(home: &Home) -> Self {
    use needy_api_client::proto::v1::runner_service_server::{RunnerService, RunnerServiceServer};
    use needy_api_client::proto::v1::{
      ControlRequest, ControlResponse, CreateJoinTokenRequest, CreateJoinTokenResponse, HeartbeatRequest, HeartbeatResponse, JoinRequest,
      JoinResponse, ListJoinTokensRequest, ListJoinTokensResponse, ListRunnersRequest, ListRunnersResponse, RemoveRunnerRequest,
      RemoveRunnerResponse, RevokeJoinTokenRequest, RevokeJoinTokenResponse, RotateRunnerRequest, RotateRunnerResponse, TaskRequest,
      TaskResponse,
    };
    use std::sync::{Arc, Mutex};
    use tonic::{Request, Response, Status, Streaming};

    struct Held {
      arrived: Mutex<std::sync::mpsc::Sender<()>>,
      release: Arc<tokio::sync::Notify>,
    }

    fn refused<T>() -> Result<T, Status> {
      Err(Status::unavailable("not part of the test"))
    }

    #[tonic::async_trait]
    impl RunnerService for Held {
      type ControlStream = tokio_stream::Empty<Result<ControlResponse, Status>>;
      type TaskStream = tokio_stream::Empty<Result<TaskResponse, Status>>;

      async fn join(&self, _: Request<JoinRequest>) -> Result<Response<JoinResponse>, Status> {
        self.arrived.lock().unwrap().send(()).unwrap();
        self.release.notified().await;
        Ok(Response::new(JoinResponse {
          runner: None,
          credential: HeldJoin::CREDENTIAL.into(),
          heartbeat_interval_ms: 60_000,
        }))
      }

      async fn create_join_token(&self, _: Request<CreateJoinTokenRequest>) -> Result<Response<CreateJoinTokenResponse>, Status> {
        refused()
      }
      async fn list_join_tokens(&self, _: Request<ListJoinTokensRequest>) -> Result<Response<ListJoinTokensResponse>, Status> {
        refused()
      }
      async fn revoke_join_token(&self, _: Request<RevokeJoinTokenRequest>) -> Result<Response<RevokeJoinTokenResponse>, Status> {
        refused()
      }
      async fn heartbeat(&self, _: Request<HeartbeatRequest>) -> Result<Response<HeartbeatResponse>, Status> {
        refused()
      }
      async fn list_runners(&self, _: Request<ListRunnersRequest>) -> Result<Response<ListRunnersResponse>, Status> {
        refused()
      }
      async fn rotate_runner(&self, _: Request<RotateRunnerRequest>) -> Result<Response<RotateRunnerResponse>, Status> {
        refused()
      }
      async fn remove_runner(&self, _: Request<RemoveRunnerRequest>) -> Result<Response<RemoveRunnerResponse>, Status> {
        refused()
      }
      async fn control(&self, _: Request<Streaming<ControlRequest>>) -> Result<Response<Self::ControlStream>, Status> {
        refused()
      }
      async fn task(&self, _: Request<Streaming<TaskRequest>>) -> Result<Response<Self::TaskStream>, Status> {
        refused()
      }
    }

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let socket = home.path().join("held.sock");
    let listener = {
      let _context = runtime.enter();
      tokio::net::UnixListener::bind(&socket).unwrap()
    };
    let (arrived_tx, arrived) = std::sync::mpsc::channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let held = Held {
      arrived: Mutex::new(arrived_tx),
      release: release.clone(),
    };
    runtime.spawn(
      tonic::transport::Server::builder()
        .add_service(RunnerServiceServer::new(held))
        .serve_with_incoming(tokio_stream::wrappers::UnixListenerStream::new(listener)),
    );
    Self {
      endpoint: format!("unix:{}", socket.display()),
      arrived,
      release,
      _runtime: runtime,
    }
  }
}

// ROOT CAUSE:
//
// If a runner daemon got a termination signal while it joined, it died of
// the signal before it stored the credential the control plane had already
// issued, because it listened for termination signals only after the join.
// The join token's use was spent and the runner could not start again.
//
// Before the fix, the runner below died of SIGTERM without `runner.json`.
// The fix listens before joining: the join ends, the credential is stored,
// then the runner stops.
#[test]
fn a_runner_daemon_stopped_while_it_joins_keeps_its_credential() {
  let machine = Home::new();
  let control = HeldJoin::start(&machine);
  let mut command = machine.needy();
  let invitation = machine.write("invitation", "a-join-token");
  {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&invitation, std::fs::Permissions::from_mode(0o600)).unwrap();
  }
  command.args([
    "--address",
    &control.endpoint,
    "runner",
    "join",
    "mac",
    "--code-file",
    invitation.to_str().unwrap(),
  ]);
  let runner = command.stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
  control.arrived.recv_timeout(Duration::from_secs(20)).expect("the runner joins");

  kill(runner.id(), "TERM");
  control.release.notify_one();
  let output = exited(runner);

  assert_eq!(output.status.code(), Some(0), "{output:?}");
  let stored = std::fs::read_to_string(machine.path().join("state").join("runner.json")).unwrap();
  let stored: serde_json::Value = serde_json::from_str(&stored).unwrap();
  assert_eq!(stored["credential"], HeldJoin::CREDENTIAL);
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

  let late = enroll_runner(&Home::new(), &control, &expired, &[]);
  let again = enroll_runner(&Home::new(), &control, &spent, &[]);

  assert_eq!(late.status.code(), Some(1));
  assert!(stderr(&late).contains("expired"), "{}", stderr(&late));
  assert_eq!(again.status.code(), Some(1));
  assert!(stderr(&again).contains("has no uses left"), "{}", stderr(&again));
}

/// Enroll with capability settings, then reconnect from the saved enrollment.
#[cfg(target_os = "linux")]
fn runner_with(machine: &Home, control: &Home, extra: &[String]) -> Child {
  let token = create_join_token(control, &[]);
  let joined = enroll_runner(machine, control, &token, extra);
  assert!(joined.status.success(), "runner join: {}", stderr(&joined));
  machine
    .needy()
    .args(["runner", "serve"])
    .args(extra)
    .env("DOCKER_HOST", format!("unix://{}", machine.path().join("no-docker.sock").display()))
    .env("PATH", path_without_apple_container())
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap()
}

// A KVM device that opens and a complete actor image give the firecracker
// driver; only Linux runs Firecracker.
#[cfg(target_os = "linux")]
#[test]
fn a_runner_with_kvm_and_the_actor_image_detects_the_linux_firecracker_driver() {
  let control = Home::new();
  let _daemon = Daemon::start(&control);
  let machine = Home::new();

  // A runner detects the image and KVM; the guest needy is the control
  // daemon's.
  let runner = RunnerDaemon(runner_with(&machine, &control, &stand_in_actor(&machine)[..4]));
  let joined = runner_when(&control, "mac", |r| r["status"] == "online" && !r["host"].is_null());
  let table = control.run(&["runner", "ls"]);
  drop(runner);

  assert_eq!(joined["capabilities"], serde_json::json!({"actor": ["linux-firecracker"], "executor": ["native"]}));
  assert!(stdout(&table).contains("actor(linux-firecracker) executor(native)"), "{}", stdout(&table));
}

#[test]
fn a_runner_restricted_to_roles_this_machine_cannot_host_warns_and_does_not_start() {
  let control = Home::new();
  let _daemon = Daemon::start(&control);
  let machine = Home::new();
  let no_kvm = machine.path().join("no-kvm").display().to_string();

  let token = create_join_token(&control, &[]);
  let refused = enroll_runner(&machine, &control, &token, &["--roles".into(), "actor".into(), "--kvm".into(), no_kvm]);

  assert_eq!(refused.status.code(), Some(1));
  let text = stderr(&refused);
  assert!(text.contains("--roles names actor, which this machine cannot host"), "{text}");
  assert!(text.contains("explicitly requested runner role is unavailable"), "{text}");
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
    let ps = control.run(&["proc", "ls", "--runner", "mac"]);
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
    "proc",
    "exec",
    "-T",
    "--runner",
    "mac",
    "--",
    "sh",
    "-c",
    "echo hi; echo oops >&2; exit 3",
  ]);
  let missing = control.run(&[
    "proc",
    "exec",
    "--runner",
    "mac",
    "--",
    "needy-no-such-command",
  ]);
  let json = control.run(&[
    "proc",
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

  let detached = control.run(&[
    "proc", "exec", "-d", "--runner", "mac", "--", "sleep", "600",
  ]);
  let id = stdout(&detached).trim().to_string();
  let listed = control.run(&["proc", "ls", "--runner", "mac", "-o", "json"]);
  let killed = control.run(&["proc", "kill", "-s", "SIGKILL", &id]);
  let copied = control.run(&[
    "fs",
    "cp",
    "--runner",
    "mac",
    source.to_str().unwrap(),
    &format!("remote:{}", destination.display()),
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

  let exec = control.run(&["proc", "exec", "--runner", "ghost", "--", "true"]);
  let ps = control.run(&["proc", "ls", "--runner", "ghost"]);

  assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
  for output in [&exec, &ps] {
    assert_eq!(output.status.code(), Some(1));
    assert!(
      stderr(output).contains("runner ghost is not connected") && stderr(output).contains("[runner.not_connected]"),
      "{}",
      stderr(output)
    );
  }
}

#[test]
fn a_control_daemon_refuses_a_guest_needy_that_is_not_a_static_linux_build() {
  let home = Home::new();
  let guest = home.write("needy-linux", "#!/bin/sh\n");

  let (exited, output) = daemon_within(&home, &["serve", "--guest-needy", &guest.display().to_string()], Duration::from_secs(20));

  assert!(exited, "the daemon started with a guest needy it cannot use");
  assert_eq!(output.status.code(), Some(1));
  assert!(stderr(&output).contains(&format!("the guest needy {}", guest.display())), "{}", stderr(&output));
  assert!(
    stderr(&output).contains("is not a 64-bit ELF executable; the guest needy is a static Linux build of needy"),
    "{}",
    stderr(&output)
  );
}

#[test]
fn only_control_serving_accepts_a_guest_needy() {
  let home = Home::new();
  let output = home.run(&[
    "daemon",
    "executor",
    "--listen",
    "unix:///tmp/example.sock",
    "--guest-needy",
    "/nonexistent",
  ]);
  assert_eq!(output.status.code(), Some(2));
  assert!(stderr(&output).contains("--guest-needy"), "{}", stderr(&output));
}

#[test]
fn a_runner_listens_nowhere() {
  let home = Home::new();
  let output = home.run(&["runner", "serve", "--listen", "tcp://127.0.0.1:0"]);
  assert_eq!(output.status.code(), Some(2));
  assert!(stderr(&output).contains("--listen"), "{}", stderr(&output));
}

#[test]
fn process_execution_cannot_combine_a_runner_with_a_run_selector() {
  let home = Home::new();
  let output = home.run(&[
    "proc", "exec", "--runner", "mac", "--run", "r1", "--", "true",
  ]);
  assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
}

/// A run whose commands all go to runner `mac`.
const RUNNER_PROFILE: &str = r#"
actor: { driver: linux-firecracker }
executors:
  mac: { runner: mac }
commands:
  - { match: "**", to: mac }
"#;

/// Waits up to 20 s for process `pid` to end.
fn gone(pid: u32) -> bool {
  let deadline = Instant::now() + Duration::from_secs(20);
  while Instant::now() < deadline {
    // Signal 0 checks that the process exists without signalling it.
    if !Command::new("kill").args(["-0", &pid.to_string()]).stderr(Stdio::null()).status().unwrap().success() {
      return true;
    }
    std::thread::sleep(Duration::from_millis(50));
  }
  false
}

#[test]
fn a_control_plane_killed_during_a_run_fails_it_at_restart_and_its_runner_ends_the_runs_processes() {
  let control = Home::new();
  let mut daemon = Daemon::start(&control);
  let machine = Home::new();
  let _runner = connected_runner(&machine, &control);
  let run = create_run(&control, RUNNER_PROFILE);
  let pid_file = machine.path().join("sleep.pid");
  let script = format!("echo $$ > {}; exec sleep 300", pid_file.display());
  detach(&control, &run, &["sh", "-c", &script]);
  let deadline = Instant::now() + Duration::from_secs(20);
  let pid = loop {
    if let Ok(pid) = std::fs::read_to_string(&pid_file).unwrap_or_default().trim().parse::<u32>() {
      break pid;
    }
    assert!(Instant::now() < deadline, "the detached process did not start");
    std::thread::sleep(Duration::from_millis(50));
  };

  // SIGKILL: the control plane ends nothing on its way out.
  daemon.child.kill().unwrap();
  daemon.child.wait().unwrap();
  let still_running = !gone_now(pid);
  // The killed daemon left its socket, which `Daemon::start` waits for.
  std::fs::remove_file(control.path().join("run").join("daemon.sock")).unwrap();
  let _restarted = Daemon::start(&control);
  let ended = gone(pid);
  let listed = control.run(&["run", "ls", "-o", "json"]);
  let table = control.run(&["run", "ls"]);

  assert!(still_running, "the runner ended the run's process as soon as the control plane died");
  assert!(ended, "process {pid} outlived its run across a control plane restart");
  let rows: serde_json::Value = serde_json::from_str(&stdout(&listed)).unwrap();
  let row = rows.as_array().unwrap().iter().find(|r| r["id"] == run.as_str()).expect("the run is listed after the restart");
  assert_eq!(row["state"], "failed");
  assert_eq!(row["failure"], "the control plane restarted; the actor was lost");
  assert!(stdout(&table).contains("failed"), "{}", stdout(&table));
}

/// Whether process `pid` is gone now.
fn gone_now(pid: u32) -> bool {
  !Command::new("kill").args(["-0", &pid.to_string()]).stderr(Stdio::null()).status().unwrap().success()
}

// ------------------------------------------------------------- clients

// ROOT CAUSE:
//
// If another test's listener took the port this helper picked, the test
// talked to that listener and failed with "transport error" or "connection
// refused", because the helper bound port 0, released the port and gave its
// number to the daemon. macOS hands out port-0 ports in sequence, so a
// daemon of another test started at the same time could get the same one.
//
// Before the fix, the daemon then failed with "Address already in use" while
// the helper's readiness loop connected to the other test's listener.
// The fix lets the daemon bind port 0 itself and takes the port from the
// daemon's own "listening on" line, so a test only talks to its own daemon.
/// A control daemon with authenticated TCP access and its independent
/// private management socket. The local operator needs no bearer.
fn spawn_control_on_tcp(home: &Home) -> (Daemon, String) {
  use std::io::BufRead as _;
  let mut child = home
    .needy()
    .args(["daemon", "serve", "-v", "--listen", "tcp://127.0.0.1:0"])
    .args(stand_in_actor(home))
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
  // The log is read to its end on another thread, so the daemon never
  // blocks on a full pipe, and the bound below holds if it logs nothing.
  let log = std::io::BufReader::new(child.stderr.take().unwrap());
  let (lines, read) = std::sync::mpsc::channel();
  std::thread::spawn(move || {
    for line in log.lines().map_while(Result::ok) {
      // The test stops listening once the daemon listens; the rest is
      // only drained.
      let _ = lines.send(line);
    }
  });
  let daemon = Daemon { child };
  let deadline = Instant::now() + Duration::from_secs(20);
  let mut seen = Vec::new();
  let listen = loop {
    let left = deadline.saturating_duration_since(Instant::now());
    let line = read.recv_timeout(left).unwrap_or_else(|e| panic!("the daemon did not listen ({e}): {seen:#?}"));
    if let Some((_, address)) = line.split_once("control daemon listening on ")
      && !address.starts_with("unix:")
    {
      break address.trim().to_string();
    }
    seen.push(line);
  };
  let deadline = Instant::now() + Duration::from_secs(20);
  loop {
    let local = home.run(&["run", "ls"]);
    if local.status.success() {
      break;
    }
    assert!(Instant::now() < deadline, "local management did not become ready: {}", stderr(&local));
    std::thread::sleep(Duration::from_millis(20));
  }
  (daemon, format!("http://{listen}"))
}

/// Pair one named local connection through a private invitation file.
fn pair_context(home: &Home, endpoint: &str, name: &str, code: &str) -> Output {
  use std::os::unix::fs::PermissionsExt as _;
  let path = home.write("invitation", code);
  std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
  home.run(&[
    "config",
    "context",
    "pair",
    name,
    "--address",
    endpoint,
    "--code-file",
    path.to_str().unwrap(),
  ])
}

/// A named network administrator bootstrapped by this control host's owner.
fn as_operator(home: &Home, endpoint: &str, args: &[&str]) -> Output {
  if !home.run(&["config", "context", "show", "administration"]).status.success() {
    let invitation = home.run(&[
      "client",
      "invite",
      "--admin",
      "--name",
      "administrator",
      "-o",
      "json",
    ]);
    assert!(invitation.status.success(), "{}", stderr(&invitation));
    let value: serde_json::Value = serde_json::from_str(&stdout(&invitation)).unwrap();
    let paired = pair_context(home, endpoint, "administration", value["code"].as_str().unwrap());
    assert!(paired.status.success(), "{}", stderr(&paired));
  }
  home.needy().args(["--context", "administration"]).args(args).output().unwrap()
}

fn as_local_operator(home: &Home, args: &[&str]) -> Output {
  home.run(args)
}

/// Explicit saved identity: never borrow a credential for a direct address.
fn as_client(home: &Home, _endpoint: &str, args: &[&str]) -> Output {
  home.needy().args(["--context", "remote"]).args(args).output().unwrap()
}

fn pairing_code(control: &Home, _endpoint: &str, name: &str) -> String {
  let created = as_local_operator(control, &["client", "invite", "--name", name, "-o", "json"]);
  assert!(created.status.success(), "{}", stderr(&created));
  let value: serde_json::Value = serde_json::from_str(&stdout(&created)).unwrap();
  value["code"].as_str().unwrap().to_string()
}

#[test]
fn a_client_pairs_stores_its_token_privately_and_uses_it_for_that_endpoint() {
  use std::os::unix::fs::PermissionsExt as _;
  let control = Home::new();
  let (_daemon, endpoint) = spawn_control_on_tcp(&control);
  let laptop = Home::new();
  // Without a token the control plane refuses the client.
  let refused = as_client(&laptop, &endpoint, &["run", "ls"]);
  assert_eq!(refused.status.code(), Some(1));

  let code = pairing_code(&control, &endpoint, "laptop");
  let paired = pair_context(&laptop, &endpoint, "remote", &code);

  assert!(paired.status.success(), "{}", stderr(&paired));
  assert!(stderr(&paired).contains("paired context remote as client"), "{}", stderr(&paired));
  let credentials = laptop.path().join("config").join("config.yaml");
  assert_eq!(std::fs::metadata(&credentials).unwrap().permissions().mode() & 0o777, 0o600);
  let stored: serde_yaml::Value = serde_yaml::from_str(&std::fs::read_to_string(&credentials).unwrap()).unwrap();
  assert!(stored["contexts"]["remote"]["credential"].as_str().unwrap().starts_with("needy-client."));
  assert_eq!(stored["current_context"].as_str(), Some("local"));
  let listed = as_client(&laptop, &endpoint, &["run", "ls"]);
  assert!(listed.status.success(), "{}", stderr(&listed));
  // The code was spent.
  let again = pair_context(&Home::new(), &endpoint, "remote", &code);
  assert_eq!(again.status.code(), Some(1));
  assert!(stderr(&again).contains("was used already"), "{}", stderr(&again));
}

// Re-pairing by name is explicit (owner decision, 2026-11-12): a code for an
// active client's name is created, with a notice that pairing with it
// replaces that client; a code without a name never takes an existing
// client's.
#[test]
fn client_invite_tells_the_operator_which_client_a_named_code_replaces() {
  let control = Home::new();
  let (_daemon, endpoint) = spawn_control_on_tcp(&control);
  let laptop = Home::new();
  let code = pairing_code(&control, &endpoint, "laptop");
  assert!(pair_context(&laptop, &endpoint, "remote", &code).status.success());
  let listed = as_local_operator(&control, &["client", "ls", "-o", "json"]);
  let clients: Vec<serde_json::Value> = serde_json::from_str(&stdout(&listed)).unwrap();
  let id = clients[0]["id"].as_str().unwrap().to_string();

  let replacing = as_operator(&control, &endpoint, &["client", "invite", "--name", "laptop", "-o", "json"]);
  let fresh = as_operator(&control, &endpoint, &["client", "invite", "--name", "desk", "-o", "json"]);
  let unnamed = as_operator(&control, &endpoint, &["client", "invite", "-o", "json"]);

  assert_eq!(replacing.status.code(), Some(0), "{}", stderr(&replacing));
  let notice = format!("pairing with this invitation replaces client laptop ({id}) and revokes its credential");
  assert!(stderr(&replacing).contains(&notice), "{}", stderr(&replacing));
  assert!(serde_json::from_str::<serde_json::Value>(&stdout(&replacing)).unwrap()["code"].as_str().unwrap().starts_with("needy-pair."));
  for created in [&fresh, &unnamed] {
    assert_eq!(created.status.code(), Some(0), "{}", stderr(created));
    assert!(!stderr(created).contains("notice:"), "{}", stderr(created));
  }
  assert!(serde_json::from_str::<serde_json::Value>(&stdout(&unnamed)).unwrap()["name"].as_str().unwrap().starts_with("client-"));
  // Pairing with the replacing code revokes the old token.
  let desk = Home::new();
  assert!(
    pair_context(
      &desk,
      &endpoint,
      "remote",
      serde_json::from_str::<serde_json::Value>(&stdout(&replacing)).unwrap()["code"].as_str().unwrap()
    )
    .status
    .success()
  );
  assert_eq!(as_client(&laptop, &endpoint, &["run", "ls"]).status.code(), Some(1));
}

#[test]
fn a_revoked_client_is_refused_and_listed_as_revoked() {
  let control = Home::new();
  let (_daemon, endpoint) = spawn_control_on_tcp(&control);
  let laptop = Home::new();
  let code = pairing_code(&control, &endpoint, "laptop");
  assert!(pair_context(&laptop, &endpoint, "remote", &code).status.success());
  let listed = as_local_operator(&control, &["client", "ls", "-o", "json"]);
  let clients: Vec<serde_json::Value> = serde_json::from_str(&stdout(&listed)).unwrap();
  let id = clients[0]["id"].as_str().unwrap().to_string();
  assert_eq!((clients[0]["name"].as_str(), clients[0]["state"].as_str()), (Some("laptop"), Some("active")));

  let revoked = as_operator(&control, &endpoint, &["client", "revoke", &id]);
  let refused = as_client(&laptop, &endpoint, &["run", "ls"]);

  assert!(revoked.status.success(), "{}", stderr(&revoked));
  assert_eq!(refused.status.code(), Some(1));
  assert!(stderr(&refused).contains("missing or invalid bearer token"), "{}", stderr(&refused));
  let text = stdout(&as_local_operator(&control, &["client", "ls"]));
  let rows = table(&text);
  assert_eq!(rows[0], ["ID", "NAME", "ACCESS", "STATE", "PAIRED", "LAST USED"]);
  assert_eq!(&rows[1][..4], [id.as_str(), "laptop", "runs", "revoked"]);
}

// A paired client drives runs but does not administer the control plane
// (owner decision, 2026-11-11).
#[test]
fn a_paired_client_cannot_create_join_tokens_remove_runners_or_manage_clients() {
  let control = Home::new();
  let (_daemon, endpoint) = spawn_control_on_tcp(&control);
  let laptop = Home::new();
  let code = pairing_code(&control, &endpoint, "laptop");
  assert!(pair_context(&laptop, &endpoint, "remote", &code).status.success());

  let token = as_client(&laptop, &endpoint, &["runner", "invite"]);
  let remove = as_client(&laptop, &endpoint, &["runner", "rm", "mac"]);
  let pair = as_client(&laptop, &endpoint, &["client", "invite", "-o", "json"]);

  for (refused, what) in [
    (&token, "creating join tokens"),
    (&remove, "removing runners"),
    (&pair, "creating pairing codes"),
  ] {
    assert_eq!(refused.status.code(), Some(1), "{}", stderr(refused));
    assert!(stderr(refused).contains(&format!("{what} needs administrator access; paired client laptop may not")), "{}", stderr(refused));
  }
  let listed = as_local_operator(&control, &["client", "ls", "-o", "json"]);
  let clients: Vec<serde_json::Value> = serde_json::from_str(&stdout(&listed)).unwrap();
  let id = clients[0]["id"].as_str().unwrap().to_string();
  let revoke = as_client(&laptop, &endpoint, &["client", "revoke", &id]);
  assert_eq!(revoke.status.code(), Some(1), "{}", stderr(&revoke));
  assert!(stderr(&revoke).contains("revoking clients needs administrator access"), "{}", stderr(&revoke));
  assert!(as_client(&laptop, &endpoint, &["run", "ls"]).status.success(), "the client was not revoked");
}

// Management grants work over authenticated networks. Runs-only identities
// keep their named principal but do not gain registry authority.
#[test]
fn network_administrators_manage_clients_while_runs_only_clients_are_denied() {
  let control = Home::new();
  let (_daemon, endpoint) = spawn_control_on_tcp(&control);
  let laptop = Home::new();
  let code = pairing_code(&control, &endpoint, "laptop");
  assert!(pair_context(&laptop, &endpoint, "remote", &code).status.success());
  for args in [&["client", "ls"][..], &["runner", "token", "ls"][..]] {
    assert!(as_local_operator(&control, args).status.success());
    let administrator = as_operator(&control, &endpoint, args);
    assert!(administrator.status.success(), "{}", stderr(&administrator));
    let client = as_client(&laptop, &endpoint, args);
    assert_eq!(client.status.code(), Some(1), "{}", stdout(&client));
    assert!(stderr(&client).contains("administrator access"), "{}", stderr(&client));
  }
  assert!(as_client(&laptop, &endpoint, &["runner", "ls"]).status.success());
}

// ROOT CAUSE:
//
// If the host was loaded, `run logs -f` printed nothing, because the test
// stopped the run half a second after it started following, and the
// stand-in Firecracker had not run its first line by then (seen: a
// `/bin/sh` of the stand-in still asleep 2.7 s after the run started, under
// eight copies of this test binary at once).
//
// Before the fix, the follow ended with the run before the console had
// anything in it.
// The fix stops the run only once `-f` has shown both console lines.
#[test]
fn report_and_run_logs_come_from_the_control_plane_and_name_the_client() {
  let control = Home::new();
  let (_daemon, endpoint) = spawn_control_on_tcp(&control);
  // The client's state directory is its own: it cannot read the control
  // plane's `trace.db`.
  let laptop = Home::new();
  let code = pairing_code(&control, &endpoint, "laptop");
  assert!(pair_context(&laptop, &endpoint, "remote", &code).status.success());
  let profile = laptop.write("review.profile.yaml", NATIVE_PROFILE);
  let created = as_client(&laptop, &endpoint, &["run", "create", "--profile", profile.to_str().unwrap()]);
  assert!(created.status.success(), "{}", stderr(&created));
  let run = stdout(&created).trim().to_string();
  assert_eq!(as_client(&laptop, &endpoint, &["run", "exec", &run, "--", "sh", "-c", "exit 2"]).status.code(), Some(2));
  // The stand-in actor prints two lines; `-f` shows them while the run runs
  // and follows until the run stops.
  const CONSOLE: &str = "boot\nhello from the actor\n";
  let mut follow = laptop.needy().args(["--context", "remote", "run", "logs", "-f", &run]).stdout(Stdio::piped()).spawn().unwrap();
  let (shown, printed) = std::sync::mpsc::channel();
  let reader = std::thread::spawn({
    use std::io::Read as _;
    let mut out = follow.stdout.take().unwrap();
    move || {
      let mut text = Vec::new();
      let mut chunk = [0u8; 256];
      while let Ok(n @ 1..) = out.read(&mut chunk) {
        text.extend_from_slice(&chunk[..n]);
        // The test may have stopped waiting; the text is joined below.
        let _ = shown.send(String::from_utf8_lossy(&text).into_owned());
      }
      String::from_utf8_lossy(&text).into_owned()
    }
  });
  let deadline = Instant::now() + Duration::from_secs(20);
  let mut seen = String::new();
  while seen != CONSOLE {
    match printed.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
      Ok(text) => seen = text,
      Err(e) => panic!("run logs -f showed {seen:?} of the console ({e})"),
    }
  }
  assert!(follow.try_wait().unwrap().is_none(), "run logs -f ended while the run runs");
  assert!(as_client(&laptop, &endpoint, &["run", "stop", &run]).status.success());
  let deadline = Instant::now() + Duration::from_secs(10);
  while follow.try_wait().unwrap().is_none() {
    assert!(Instant::now() < deadline, "run logs -f did not end with the run");
    std::thread::sleep(Duration::from_millis(50));
  }
  let followed = reader.join().unwrap();

  let human = as_client(&laptop, &endpoint, &["run", "report", &run]);
  let json = as_client(&laptop, &endpoint, &["run", "report", &run[..6], "-o", "json"]);
  let local = laptop.run(&["run", "report", "--offline", &run]);

  assert_eq!(followed, CONSOLE);
  assert!(human.status.success(), "{}", stderr(&human));
  let report = stdout(&human);
  assert!(report.contains("\nStarted by  client:laptop\n"), "{report}");
  assert!(report.contains("\nStopped by  client:laptop\n"), "{report}");
  assert!(report.contains("routed to here   1  (1 failed)"), "{report}");
  let summary: serde_json::Value = serde_json::from_str(&stdout(&json)).unwrap();
  assert_eq!((summary["run"].as_str(), summary["state"].as_str()), (Some(run.as_str()), Some("stopped")));
  assert_eq!((summary["started_by"].as_str(), summary["stopped_by"].as_str()), (Some("client:laptop"), Some("client:laptop")));
  assert_eq!(local.status.code(), Some(1), "the client has no trace.db of its own");
  assert!(stderr(&local).contains("no trace database"), "{}", stderr(&local));
}

#[test]
fn the_operator_is_named_in_the_report_of_a_run_it_started() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let run = create_run(&home, NATIVE_PROFILE);
  assert!(home.run(&["run", "stop", &run]).status.success());

  let json = home.run(&["run", "report", &run, "-o", "json"]);

  let summary: serde_json::Value = serde_json::from_str(&stdout(&json)).unwrap();
  assert_eq!((summary["started_by"].as_str(), summary["stopped_by"].as_str()), (Some("operator"), Some("operator")));
}

#[test]
fn pairing_is_local_configuration_and_authorization_is_a_separate_group() {
  let home = Home::new();
  assert_eq!(home.run(&["pair", "--help"]).status.code(), Some(2));
  let help = home.run(&["client", "--help"]);
  assert!(help.status.success(), "{}", stderr(&help));
  for command in ["invite", "ls", "revoke"] {
    assert!(stdout(&help).contains(command));
  }
  assert!(home.run(&["config", "context", "pair", "--help"]).status.success());
}

#[test]
fn pairing_preserves_local_selection_and_failed_explicit_targets_never_fall_back() {
  let control = Home::new();
  let (_daemon, endpoint) = spawn_control_on_tcp(&control);
  let laptop = Home::new();
  let code = pairing_code(&control, &endpoint, "laptop");
  assert!(pair_context(&laptop, &endpoint, "remote", &code).status.success());
  assert_eq!(laptop.run(&["run", "ls"]).status.code(), Some(1), "pairing did not select a remote default");
  assert!(laptop.run(&["config", "use", "remote"]).status.success());
  assert!(laptop.run(&["run", "ls"]).status.success());
  let failed = laptop.needy().args(["--address", "http://127.0.0.1:9", "run", "ls"]).output().unwrap();
  assert_eq!(failed.status.code(), Some(1));
  assert!(stderr(&failed).contains("http://127.0.0.1:9"), "{}", stderr(&failed));
  let _local = Daemon::start(&laptop);
  let local = laptop.run(&["--context", "local", "client", "ls", "-o", "json"]);
  assert!(local.status.success(), "{}", stderr(&local));
  assert_eq!(serde_json::from_str::<Vec<serde_json::Value>>(&stdout(&local)).unwrap().len(), 0);
  assert!(laptop.run(&["run", "ls"]).status.success(), "remote selection survived local serving");
}

/// A run of `NATIVE_PROFILE` whose profile is named `name`.
fn named_run(home: &Home, name: &str) -> String {
  let profile = home.write(&format!("{name}.profile.yaml"), NATIVE_PROFILE);
  let output = home.run(&["run", "create", "--profile", profile.to_str().unwrap()]);
  assert!(output.status.success(), "run create: {}", stderr(&output));
  stdout(&output).trim().to_string()
}

/// The state `run ls` shows for `run`.
fn run_state(home: &Home, run: &str) -> String {
  let listed = home.run(&["run", "ls", "-o", "json"]);
  let runs: Vec<serde_json::Value> = serde_json::from_str(&stdout(&listed)).unwrap();
  runs.iter().find(|r| r["id"] == run).map(|r| r["state"].as_str().unwrap().to_string()).unwrap_or_default()
}

// A command about a run that names none takes the only live run (owner
// decision, 2026-11-12), so a run ID need not be copied by hand.
#[test]
fn a_run_command_without_a_run_takes_the_only_live_run() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);

  let none = home.run(&["run", "stop"]);
  let stopped = named_run(&home, "old");
  assert!(home.run(&["run", "stop", &stopped]).status.success());
  let live = named_run(&home, "solo");
  let logs = home.run(&["run", "logs"]);
  let stop = home.run(&["run", "stop"]);

  assert_eq!(none.status.code(), Some(1));
  assert!(stderr(&none).contains("no run is live"), "{}", stderr(&none));
  assert!(logs.status.success(), "{}", stderr(&logs));
  assert!(stop.status.success(), "{}", stderr(&stop));
  assert_eq!(run_state(&home, &live), "stopped");
}

#[test]
fn a_run_command_without_a_run_lists_the_live_runs_when_there_are_several() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let first = named_run(&home, "first");
  let second = named_run(&home, "second");

  let output = home.run(&["run", "stop"]);

  assert_eq!(output.status.code(), Some(1));
  let message = stderr(&output);
  assert!(message.contains("2 runs are live"), "{message}");
  for (run, profile) in [(&first, "first"), (&second, "second")] {
    let line = message.lines().find(|l| l.contains(run.as_str())).unwrap_or_else(|| panic!("{message}"));
    assert!(line.contains(profile) && line.contains(" ago"), "{line}");
    assert_eq!(run_state(&home, run), "running");
  }
}

// Run selection accepts stable IDs, latest, prefixes and an inherited nonsecret
// execution reference. Explicit process execution remains outside that run.
#[test]
fn run_accepts_latest_a_prefix_and_inherited_execution_selection() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let first = named_run(&home, "first");
  let second = named_run(&home, "second");
  let inherited = serde_json::json!({
    "version":1, "config_file":home.path().join("config/config.yaml"),
    "context":"local", "address":format!("unix:{}", home.path().join("run/daemon.sock").display()),
    "command":null, "run":first
  })
  .to_string();
  let detached = |selector: &[&str], inherited: Option<&str>| {
    let mut command = home.needy();
    if let Some(reference) = inherited {
      command.env("NEEDY_EXECUTION_CONTEXT", reference);
    }
    let output = command.args(["run", "exec"]).args(selector).args(["-d", "--", "sleep", "30"]).output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    stdout(&output).trim().to_string()
  };
  let latest = detached(&["latest"], None);
  let prefix = detached(&[&first[..11]], None);
  let selected = detached(&[], Some(&inherited));
  let outside =
    home.needy().env("NEEDY_EXECUTION_CONTEXT", &inherited).args(["proc", "exec", "--host", "-d", "--", "sleep", "30"]).output().unwrap();
  assert!(outside.status.success(), "{}", stderr(&outside));
  let outside = stdout(&outside).trim().to_string();
  let source = home.write("note.txt", "hello");
  let dir = home.path().join("copied");
  let copied = home.run(&[
    "fs",
    "cp",
    "--run",
    &second[..11],
    "--executor",
    "here",
    source.to_str().unwrap(),
    &format!("remote:{}", dir.display()),
  ]);
  let report = home.needy().env("NEEDY_EXECUTION_CONTEXT", &inherited).args(["run", "report", "-o", "json"]).output().unwrap();
  let in_latest = home.run(&["proc", "ls", "--run", "latest", "-o", "json"]);
  let listed = home.run(&["proc", "ls", "-o", "json"]);
  let processes: Vec<serde_json::Value> = serde_json::from_str(&stdout(&listed)).unwrap();
  let run_of = |id: &str| processes.iter().find(|p| p["id"] == id).map(|p| p["run"].as_str().unwrap().to_string());
  assert_eq!(run_of(&latest).as_deref(), Some(second.as_str()));
  assert_eq!(run_of(&prefix).as_deref(), Some(first.as_str()));
  assert_eq!(run_of(&selected).as_deref(), Some(first.as_str()));
  assert_eq!(run_of(&outside).as_deref(), Some(""));
  assert!(copied.status.success(), "{}", stderr(&copied));
  assert_eq!(std::fs::read_to_string(dir.join("note.txt")).unwrap(), "hello");
  let summary: serde_json::Value = serde_json::from_str(&stdout(&report)).unwrap();
  assert_eq!(summary["run"].as_str(), Some(first.as_str()));
  let in_latest: Vec<serde_json::Value> = serde_json::from_str(&stdout(&in_latest)).unwrap();
  assert!(in_latest.iter().all(|p| p["run"] == second.as_str()));
  assert!(in_latest.iter().any(|p| p["id"] == latest.as_str()));
  assert!(home.run(&["proc", "kill", &outside, "-s", "SIGKILL"]).status.success());
  for run in [&first, &second] {
    assert!(home.run(&["run", "stop", run]).status.success());
  }
}

// `--profile NAME`, without a path separator or `.yaml`, is
// `profiles/NAME.yaml` in the config directory (owner decision, 2026-11-12).
#[test]
fn a_profile_is_found_by_name_in_the_config_directory() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let profiles = home.path().join("config").join("profiles");
  std::fs::create_dir_all(&profiles).unwrap();
  std::fs::write(profiles.join("named.yaml"), NATIVE_PROFILE).unwrap();

  let created = home.run(&["run", "create", "--profile", "named"]);
  let missing = home.run(&["run", "create", "--profile", "absent"]);

  assert!(created.status.success(), "{}", stderr(&created));
  let run = stdout(&created).trim().to_string();
  let report = home.run(&["run", "report", &run, "-o", "markdown"]);
  assert_eq!(stdout(&report).lines().next(), Some(format!("# Run report: {run} (profile: named)").as_str()));
  assert_eq!(missing.status.code(), Some(1));
  let absent = profiles.join("absent.yaml");
  assert!(stderr(&missing).contains(absent.to_str().unwrap()), "{}", stderr(&missing));
  assert!(home.run(&["run", "stop", &run]).status.success());
}

#[test]
fn run_execution_keeps_every_word_after_the_separator_in_argv() {
  let home = Home::new();
  let _daemon = Daemon::start(&home);
  let profile = home.write("argv.yaml", NATIVE_PROFILE);
  let created = home.run(&[
    "run",
    "create",
    "--profile",
    profile.to_str().unwrap(),
    "--name",
    "demo",
  ]);
  assert!(created.status.success(), "{}", stderr(&created));
  let omitted = home.run(&["run", "exec", "--", "echo", "hi"]);
  assert_eq!(omitted.status.code(), Some(0), "{}", stderr(&omitted));
  assert_eq!(omitted.stdout, b"hi\n");
  let named = home.run(&["run", "exec", "demo", "--", "printf", "%s", "--help"]);
  assert_eq!(named.status.code(), Some(0), "{}", stderr(&named));
  assert_eq!(named.stdout, b"--help");
  let subcommand = home.run(&["run", "exec", "demo", "--", "ls", "-d", "."]);
  assert_eq!(subcommand.status.code(), Some(0), "{}", stderr(&subcommand));
  assert_eq!(subcommand.stdout, b".\n");
  let missing_separator = home.run(&["run", "exec", "demo", "echo", "hi"]);
  assert_eq!(missing_separator.status.code(), Some(2));
}
