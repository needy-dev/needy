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
