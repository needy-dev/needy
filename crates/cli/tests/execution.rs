//! Execution selection and argv boundaries through the real CLI.

#![cfg(unix)]

use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

fn needy(home: &Path) -> Command {
  let mut command = Command::new(env!("CARGO_BIN_EXE_needy"));
  for name in [
    "NEEDY_ENDPOINT",
    "NEEDY_TOKEN",
    "NEEDY_RUN",
    "NEEDY_EXECUTION_CONTEXT",
  ] {
    command.env_remove(name);
  }
  for (variable, directory) in [
    ("NEEDY_CONFIG_DIR", "config"),
    ("NEEDY_STATE_DIR", "state"),
    ("NEEDY_DATA_DIR", "data"),
    ("NEEDY_RUNTIME_DIR", "runtime"),
  ] {
    command.env(variable, home.join(directory));
  }
  command.stdin(Stdio::null());
  command
}

struct Daemon(Child);

impl Daemon {
  fn start(home: &Path) -> Self {
    let child = needy(home).args(["daemon", "serve"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let mut daemon = Self(child);
    let deadline = Instant::now() + Duration::from_secs(20);
    while !home.join("runtime/control.json").exists() {
      assert!(daemon.0.try_wait().unwrap().is_none(), "daemon exited before discovery publication");
      assert!(Instant::now() < deadline, "daemon did not publish discovery");
      std::thread::sleep(Duration::from_millis(25));
    }
    daemon
  }
}

impl Drop for Daemon {
  fn drop(&mut self) {
    if self.0.kill().is_ok() {
      self.0.wait().unwrap();
    }
  }
}

fn stderr(output: &Output) -> String {
  String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn explicit_host_process_preserves_child_exit_and_ignores_ambient_run() {
  let home = tempfile::Builder::new().prefix("nee-").tempdir().unwrap();
  let _daemon = Daemon::start(home.path());
  let output = needy(home.path())
    .env("NEEDY_RUN", "unrelated-run")
    .args([
      "proc",
      "exec",
      "--host",
      "--",
      "sh",
      "-c",
      "printf 'host output'; exit 7",
    ])
    .output()
    .unwrap();
  assert_eq!(output.status.code(), Some(7), "{}", stderr(&output));
  assert_eq!(output.stdout, b"host output");
}

#[tokio::test]
async fn offline_report_rejects_an_ambiguous_prefix_without_contacting_a_daemon() {
  use needy_trace::Sink as _;
  let home = tempfile::Builder::new().prefix("nee-").tempdir().unwrap();
  std::fs::create_dir(home.path().join("state")).unwrap();
  let store = needy_trace::sink::sqlite::Store::open(&home.path().join("state/trace.db")).await.unwrap();
  for (id, time) in [("abc1", 10), ("abc2", 20)] {
    store
      .sink()
      .write(&needy_trace::Event {
        run: id.into(),
        seq: 0,
        time_ms: time,
        kind: "trace.gap".into(),
        version: needy_core::event::SCHEMA_VERSION,
        payload: serde_json::value::RawValue::from_string("{\"dropped\":1}".into()).unwrap(),
      })
      .await
      .unwrap();
  }
  let output = needy(home.path()).args(["run", "report", "abc", "--offline"]).output().unwrap();
  assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
  assert!(stderr(&output).contains("ambiguous"), "{}", stderr(&output));
  let exact = needy(home.path()).args(["run", "report", "abc1", "--offline"]).output().unwrap();
  assert!(exact.status.success(), "{}", stderr(&exact));
  assert!(String::from_utf8_lossy(&exact.stdout).contains("abc1"));
}

#[test]
fn a_child_keeps_its_connection_when_the_saved_default_changes() {
  use std::os::unix::fs::PermissionsExt as _;
  let home = tempfile::Builder::new().prefix("nee-").tempdir().unwrap();
  let _daemon = Daemon::start(home.path());
  std::fs::create_dir_all(home.path().join("config")).unwrap();
  let config = home.path().join("config/config.yaml");
  std::fs::write(&config, "version: 1\ncurrent_context: local\ncontexts:\n  other:\n    address: tcp://127.0.0.1:1\n    identity: { client_id: test-client, name: test-client, access: runs, paired_at_ms: 1 }\n    credential: test-credential\n").unwrap();
  std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
  let executable = env!("CARGO_BIN_EXE_needy");
  let output = needy(home.path())
    .args([
      "proc",
      "exec",
      "--host",
      "--",
      "sh",
      "-c",
      "\"$1\" config use other && exec \"$1\" runner ls -o json",
      "_",
      executable,
    ])
    .output()
    .unwrap();
  assert!(output.status.success(), "{}", stderr(&output));
  assert_eq!(serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(), serde_json::json!([]));
  let saved = needy(home.path()).args(["config", "get", "current_context", "-o", "json"]).output().unwrap();
  assert!(saved.status.success(), "{}", stderr(&saved));
  assert_eq!(serde_json::from_slice::<serde_json::Value>(&saved.stdout).unwrap(), "other");
}

#[test]
fn host_copy_round_trips_a_local_filename_containing_a_colon() {
  let home = tempfile::Builder::new().prefix("nee-").tempdir().unwrap();
  let _daemon = Daemon::start(home.path());
  let source = home.path().join("input:name");
  let destination = home.path().join("destination");
  let downloaded = home.path().join("downloaded");
  std::fs::write(&source, b"copied through an explicit host target\n").unwrap();
  let remote = format!("remote:{}", destination.display());
  let upload = needy(home.path()).args(["fs", "cp", "--host", source.to_str().unwrap(), &remote]).output().unwrap();
  assert!(upload.status.success(), "{}", stderr(&upload));
  assert_eq!(std::fs::read(destination.join("input:name")).unwrap(), std::fs::read(&source).unwrap());
  let remote_file = format!("remote:{}", destination.join("input:name").display());
  let download = needy(home.path())
    .args([
      "fs",
      "cp",
      "--host",
      &remote_file,
      downloaded.to_str().unwrap(),
    ])
    .output()
    .unwrap();
  assert!(download.status.success(), "{}", stderr(&download));
  assert_eq!(std::fs::read(downloaded.join("input:name")).unwrap(), std::fs::read(&source).unwrap());
  let invalid = needy(home.path()).args(["fs", "cp", "--host", &remote, &remote_file]).output().unwrap();
  assert!(!invalid.status.success());
  assert!(stderr(&invalid).contains("files.invalid_target"));
}

#[test]
fn a_completed_child_exit_255_is_not_an_unknown_outcome() {
  let home = tempfile::Builder::new().prefix("nee-").tempdir().unwrap();
  let _daemon = Daemon::start(home.path());
  let output = needy(home.path())
    .args([
      "--diagnostic-format",
      "json",
      "proc",
      "exec",
      "--host",
      "--output",
      "json",
      "--",
      "sh",
      "-c",
      "printf completed; exit 255",
    ])
    .output()
    .unwrap();
  assert_eq!(output.status.code(), Some(255));
  let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  assert_eq!(result["stdout"], "completed");
  assert_eq!(result["exit_code"], 255);
  assert!(result.get("error").is_none());
  assert!(output.stderr.is_empty());
}

#[test]
fn legacy_endpoint_cannot_silently_redirect_a_request_to_local_management() {
  let home = tempfile::Builder::new().prefix("nee-").tempdir().unwrap();
  let _daemon = Daemon::start(home.path());
  let output =
    needy(home.path()).env("NEEDY_ENDPOINT", "http://127.0.0.1:9").args(["--diagnostic-format", "json", "runner", "ls"]).output().unwrap();
  assert!(!output.status.success(), "legacy remote selection must not become local operator access");
  let diagnostic: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
  assert_eq!(diagnostic["reason"], "config.selection_conflict");
  let explicit = needy(home.path())
    .env("NEEDY_ENDPOINT", "http://127.0.0.1:9")
    .args(["--context", "local", "runner", "ls", "-o", "json"])
    .output()
    .unwrap();
  assert!(explicit.status.success(), "{}", stderr(&explicit));
  assert_eq!(serde_json::from_slice::<serde_json::Value>(&explicit.stdout).unwrap(), serde_json::json!([]));
}
