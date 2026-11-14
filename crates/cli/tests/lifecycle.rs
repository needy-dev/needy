//! Local lifecycle checks at the real CLI process/output/filesystem boundary.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Home(tempfile::TempDir);
impl Home {
  fn new() -> Self {
    Self(tempfile::Builder::new().prefix("needy-lc-").tempdir_in(std::env::temp_dir()).unwrap())
  }
  fn path(&self) -> &Path {
    self.0.path()
  }
  fn command(&self) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_needy"));
    command
      .env_remove("NEEDY_ENDPOINT")
      .env_remove("NEEDY_TOKEN")
      .env_remove("NEEDY_EXECUTION_CONTEXT")
      .env("NEEDY_CONFIG_DIR", self.path().join("config"))
      .env("NEEDY_STATE_DIR", self.path().join("state"))
      .env("NEEDY_DATA_DIR", self.path().join("data"))
      .env("NEEDY_RUNTIME_DIR", self.path().join("run"))
      .env("HOME", self.path())
      .env("XDG_CONFIG_HOME", self.path().join("config-base"))
      .stdin(Stdio::null());
    command
  }
}
struct Serving(Child);
impl Drop for Serving {
  fn drop(&mut self) {
    if self.0.kill().is_ok() {
      self.0.wait().unwrap();
    }
  }
}

#[test]
fn starting_an_uninstalled_service_reports_installation_guidance() {
  let home = Home::new();
  let output = home.command().args(["--diagnostic-format", "json", "daemon", "service", "start"]).output().unwrap();
  assert!(!output.status.success());
  assert!(output.stdout.is_empty());
  let diagnostic: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
  assert_eq!(diagnostic["reason"], "service.not_installed");
  assert!(
    diagnostic["checks"]
      .as_array()
      .unwrap()
      .iter()
      .any(|check| check["argv"] == serde_json::json!(["needy", "daemon", "service", "install"]))
  );
  assert!(!home.path().join("config/services").exists(), "inspection must not create an installation");
}

#[tokio::test]
async fn network_control_keeps_private_management_discovery_and_cleans_its_record() {
  let home = Home::new();
  let mut daemon = Serving(
    home.command().args(["daemon", "serve", "--listen", "tcp://127.0.0.1:0"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap(),
  );
  let runtime = home.path().join("run");
  let deadline = Instant::now() + Duration::from_secs(20);
  let record = loop {
    if let Ok(record) = needy_daemon::discovery::read(&runtime) {
      break record;
    }
    assert!(daemon.0.try_wait().unwrap().is_none(), "control serving exited before discovery publication");
    assert!(Instant::now() < deadline, "control discovery was not published");
    tokio::time::sleep(Duration::from_millis(20)).await;
  };
  assert_eq!(record.pid, daemon.0.id());
  assert_eq!(std::fs::metadata(runtime.join("control.json")).unwrap().permissions().mode() & 0o777, 0o600);
  needy_api_client::ready::wait_expected(&record.address, Duration::from_secs(2), "control", &record.instance_id).await.unwrap();
  let listed = home.command().args(["runner", "token", "ls", "-o", "json"]).output().unwrap();
  assert!(listed.status.success(), "{}", String::from_utf8_lossy(&listed.stderr));
  assert_eq!(serde_json::from_slice::<serde_json::Value>(&listed.stdout).unwrap(), serde_json::json!([]));
  let competing = home.command().args(["daemon", "serve"]).output().unwrap();
  assert!(!competing.status.success());
  assert!(String::from_utf8_lossy(&competing.stderr).contains("Another control daemon owns"));
  assert_eq!(needy_daemon::discovery::read(&runtime).unwrap().instance_id, record.instance_id);
  assert!(Command::new("kill").args(["-TERM", &daemon.0.id().to_string()]).status().unwrap().success());
  let deadline = Instant::now() + Duration::from_secs(10);
  while daemon.0.try_wait().unwrap().is_none() {
    assert!(Instant::now() < deadline, "graceful control shutdown did not finish");
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
  assert!(!runtime.join("control.json").exists());
  assert!(!runtime.join("daemon.sock").exists());
}

#[test]
fn service_install_snapshots_settings_without_starting_and_refuses_foreign_definition() {
  let home = Home::new();
  let bin = home.path().join("bin");
  std::fs::create_dir(&bin).unwrap();
  let calls = home.path().join("calls");
  let name = if cfg!(target_os = "macos") {
    "launchctl"
  } else {
    "systemctl"
  };
  // The OS-manager executable is the external boundary. No live manager or
  // unrelated service is changed by this installation test.
  needy_common::testing::write_executable(&bin.join(name), &format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in *show*) printf 'LoadState=not-found\\nActiveState=inactive\\nMainPID=0\\n';; esac\n", calls.display())).unwrap();
  let output = home.command().env("PATH", &bin).args(["daemon", "service", "install"]).output().unwrap();
  assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
  let calls = std::fs::read_to_string(calls).unwrap();
  assert!(!calls.lines().any(|line| line.starts_with("bootstrap ") || line.split_whitespace().any(|word| word == "start")), "{calls}");
  let receipt = home.path().join("config/services/user/registration.json");
  let registration: serde_json::Value = serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
  let snapshot = Path::new(registration["snapshot"].as_str().unwrap());
  let saved: serde_yaml::Value = serde_yaml::from_slice(&std::fs::read(snapshot).unwrap()).unwrap();
  assert_eq!(saved["current_context"].as_str(), Some("local"));
  assert_eq!(saved["contexts"].as_mapping().unwrap().len(), 0);
  assert_eq!(registration["path"].as_str().unwrap(), bin.to_str().unwrap());
  let definition = Path::new(registration["definition"].as_str().unwrap());
  std::fs::write(definition, "foreign service definition\n").unwrap();
  let replaced = home.command().env("PATH", &bin).args(["daemon", "service", "install", "--replace"]).output().unwrap();
  assert!(!replaced.status.success());
  assert_eq!(std::fs::read_to_string(definition).unwrap(), "foreign service definition\n");
  assert!(snapshot.exists(), "configuration snapshots survive failed replacement");
}

#[tokio::test]
async fn service_readiness_does_not_adopt_a_healthy_manual_daemon() {
  let home = Home::new();
  let mut manual = Serving(home.command().args(["daemon", "serve"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());
  let deadline = Instant::now() + Duration::from_secs(20);
  while needy_daemon::discovery::read(&home.path().join("run")).is_err() {
    assert!(manual.0.try_wait().unwrap().is_none());
    assert!(Instant::now() < deadline);
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
  let bin = home.path().join("bin");
  std::fs::create_dir(&bin).unwrap();
  let marker = home.path().join("manager-running");
  let (name, script) = if cfg!(target_os = "macos") {
    (
      "launchctl",
      format!(
        "#!/bin/sh\ncase \"$2\" in gui/*/dev.needy.control) printf 'path = %s\\nprogram = %s\\n' '{}' '{}'; exit 0;; esac\nif [ -e '{}' ]; then printf '1 - dev.needy.control\\n'; fi\n",
        home.path().join("Library/LaunchAgents/dev.needy.control.plist").display(),
        env!("CARGO_BIN_EXE_needy"),
        marker.display()
      ),
    )
  } else {
    (
      "systemctl",
      format!(
        "#!/bin/sh\ncase \"$*\" in *FragmentPath*) printf 'FragmentPath=%s\\n' '{}'; exit 0;; esac\nif [ -e '{}' ]; then printf 'LoadState=loaded\\nActiveState=active\\nMainPID=1\\n'; else printf 'LoadState=not-found\\nActiveState=inactive\\nMainPID=0\\n'; fi\n",
        home.path().join("config-base/systemd/user/needy.service").display(),
        marker.display()
      ),
    )
  };
  needy_common::testing::write_executable(&bin.join(name), &script).unwrap();
  let installed = home.command().env("PATH", &bin).args(["daemon", "service", "install"]).output().unwrap();
  assert!(installed.status.success(), "{}", String::from_utf8_lossy(&installed.stderr));
  std::fs::write(marker, "manager PID intentionally differs from manual child\n").unwrap();
  let started = home
    .command()
    .env("PATH", &bin)
    .args([
      "--diagnostic-format",
      "json",
      "daemon",
      "service",
      "start",
      "--timeout",
      "200ms",
    ])
    .output()
    .unwrap();
  assert!(!started.status.success(), "a healthy manual daemon cannot satisfy managed readiness");
  let diagnostic: serde_json::Value = serde_json::from_slice(&started.stderr).unwrap();
  assert_eq!(diagnostic["reason"], "connection.timeout");
  assert!(manual.0.try_wait().unwrap().is_none(), "the service command must not signal the manual daemon");
  let record = needy_daemon::discovery::read(&home.path().join("run")).unwrap();
  assert_eq!(record.pid, manual.0.id());
}
