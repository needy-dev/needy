//! Service identity is verified at the CLI/OS-manager process boundary.
#![cfg(target_os = "macos")]

use std::path::Path;
use std::process::{Command, Output};

fn command(home: &Path, args: &[&str]) -> Command {
  let path =
    std::env::join_paths(std::iter::once(home.join("bin")).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap()))).unwrap();
  let mut command = Command::new(env!("CARGO_BIN_EXE_needy"));
  command
    .args(args)
    .env("HOME", home)
    .env("PATH", path)
    .env("NEEDY_CONFIG_DIR", home.join("config"))
    .env("NEEDY_STATE_DIR", home.join("state"))
    .env("NEEDY_DATA_DIR", home.join("data"))
    .env("NEEDY_RUNTIME_DIR", home.join("runtime"))
    .env("NEEDY_TEST_SERVICE_HOME", home)
    .env_remove("NEEDY_ENDPOINT")
    .env_remove("NEEDY_TOKEN")
    .env_remove("NEEDY_EXECUTION_CONTEXT");
  command
}

fn needy(home: &Path, args: &[&str]) -> Output {
  command(home, args).output().unwrap()
}

#[test]
fn a_foreign_manager_binding_is_not_stopped_using_an_old_owned_receipt() {
  let home = tempfile::tempdir().unwrap();
  std::fs::create_dir(home.path().join("bin")).unwrap();
  // This is a recorded manager boundary fixture, not an actual OS job. The
  // private owned definition remains untouched while a foreign canonical
  // label becomes loaded from another definition.
  let script = r#"#!/bin/sh
case "$1:$2" in
  print:gui/*/dev.needy.control)
    echo 'path = /foreign/definition.plist'
    echo 'program = /foreign/program'
    echo 'arguments = {'
    echo ' /foreign/program'
    echo '}'
    ;;
  print:gui/*)
    if [ -f "$NEEDY_TEST_SERVICE_HOME/loaded" ]; then
      echo ' 12345 0 dev.needy.control'
    fi
    ;;
  bootout:*)
    echo stopped >> "$NEEDY_TEST_SERVICE_HOME/mutations"
    rm -f "$NEEDY_TEST_SERVICE_HOME/loaded"
    ;;
esac
exit 0
"#;
  needy_common::testing::write_executable(&home.path().join("bin/launchctl"), script).unwrap();
  let installed = needy(home.path(), &["daemon", "service", "install"]);
  assert!(installed.status.success(), "{}", String::from_utf8_lossy(&installed.stderr));
  std::fs::write(home.path().join("loaded"), "foreign manager binding").unwrap();
  let stopped = needy(
    home.path(),
    &[
      "--diagnostic-format",
      "json",
      "daemon",
      "service",
      "stop",
      "--timeout",
      "2s",
    ],
  );
  assert!(!stopped.status.success());
  assert!(!home.path().join("mutations").exists(), "the foreign manager job was stopped");
  assert!(String::from_utf8_lossy(&stopped.stderr).contains("service.definition_conflict"), "{}", String::from_utf8_lossy(&stopped.stderr));
}

#[test]
fn concurrent_service_installations_cannot_publish_different_generations() {
  use std::time::{Duration, Instant};
  let home = tempfile::tempdir().unwrap();
  std::fs::create_dir(home.path().join("bin")).unwrap();
  let script = r#"#!/bin/sh
if [ "$NEEDY_TEST_FIRST" = yes ] && [ "$1" = print ]; then
  touch "$NEEDY_TEST_SERVICE_HOME/entered"
  while [ ! -e "$NEEDY_TEST_SERVICE_HOME/release" ]; do sleep 0.02; done
fi
exit 0
"#;
  needy_common::testing::write_executable(&home.path().join("bin/launchctl"), script).unwrap();
  let mut first = command(home.path(), &["daemon", "service", "install"])
    .env("NEEDY_TEST_FIRST", "yes")
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
  let deadline = Instant::now() + Duration::from_secs(10);
  while !home.path().join("entered").exists() {
    assert!(first.try_wait().unwrap().is_none());
    assert!(Instant::now() < deadline);
    std::thread::sleep(Duration::from_millis(10));
  }
  let second = command(
    home.path(),
    &[
      "--diagnostic-format",
      "json",
      "daemon",
      "service",
      "install",
    ],
  )
  .env("NEEDY_CONFIG_DIR", home.path().join("other-config"))
  .output()
  .unwrap();
  let stopped = needy(home.path(), &["--diagnostic-format", "json", "daemon", "service", "stop"]);
  std::fs::write(home.path().join("release"), "resume the owned installation").unwrap();
  let first = first.wait_with_output().unwrap();
  assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
  assert!(!second.status.success(), "a concurrent installation replaced a generation before it was published");
  let diagnostic: serde_json::Value = serde_json::from_slice(&second.stderr).unwrap();
  assert_eq!(diagnostic["reason"], "service.definition_conflict");
  assert!(!stopped.status.success());
  assert_eq!(serde_json::from_slice::<serde_json::Value>(&stopped.stderr).unwrap()["reason"], "service.definition_conflict");
  let directory = home.path().join("config/services/user");
  let snapshots = std::fs::read_dir(&directory)
    .unwrap()
    .filter_map(Result::ok)
    .filter(|entry| entry.path().extension().is_some_and(|extension| extension == "yaml"))
    .count();
  assert_eq!(snapshots, 1);
}
