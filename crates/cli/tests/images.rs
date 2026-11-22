//! Image validation reports the failed artifact without replacing an installed base.
#![cfg(unix)]

use std::process::Command;

#[test]
fn a_checksum_mismatch_reports_a_typed_diagnostic_and_preserves_the_base() {
  let home = tempfile::tempdir().unwrap();
  let archive = home.path().join("image.tar.gz");
  std::fs::write(&archive, b"synthetic invalid image archive").unwrap();
  std::fs::write(home.path().join("image.tar.gz.sha256"), "0".repeat(64)).unwrap();
  let base = home.path().join("data/images/macos-vz/base");
  std::fs::create_dir_all(&base).unwrap();
  std::fs::write(base.join("retained"), "installed image").unwrap();
  let output = Command::new(env!("CARGO_BIN_EXE_needy"))
    .env("NEEDY_CONFIG_DIR", home.path().join("config"))
    .env("NEEDY_DATA_DIR", home.path().join("data"))
    // Keep tests offline: never auto-pull release archives from GitHub.
    .env("NEEDY_DOWNLOAD_BASE", "http://127.0.0.1:1")
    .env_remove("NEEDY_EXECUTION_CONTEXT")
    .args([
      "--diagnostic-format",
      "json",
      "image",
      "pull",
      "--driver",
      "macos-vz",
      "--file",
      archive.to_str().unwrap(),
    ])
    .output()
    .unwrap();
  assert!(!output.status.success());
  let diagnostic: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
  assert_eq!(diagnostic["reason"], "image.integrity_failed");
  assert_eq!(diagnostic["facts"]["artifact"], std::fs::canonicalize(&archive).unwrap().display().to_string());
  assert!(diagnostic["summary"].as_str().unwrap().contains("this image was not installed"));
  assert_eq!(std::fs::read_to_string(base.join("retained")).unwrap(), "installed image");
  assert!(output.stdout.is_empty());
}

#[test]
fn image_pull_failure_emits_json_diagnostic() {
  let home = tempfile::tempdir().unwrap();
  let output = Command::new(env!("CARGO_BIN_EXE_needy"))
    .env("NEEDY_CONFIG_DIR", home.path().join("config"))
    .env("NEEDY_DATA_DIR", home.path().join("data"))
    .env("NEEDY_DOWNLOAD_BASE", "http://127.0.0.1:1")
    .env_remove("NEEDY_EXECUTION_CONTEXT")
    .args([
      "--diagnostic-format",
      "json",
      "image",
      "pull",
      "--driver",
      "macos-vz",
    ])
    .output()
    .unwrap();
  assert_eq!(output.status.code(), Some(1));
  assert!(output.stdout.is_empty());
  let diagnostic: serde_json::Value = serde_json::from_slice(&output.stderr).expect("valid json diagnostic");
  assert_eq!(diagnostic["reason"], "image.pull_failed");
  assert_eq!(diagnostic["facts"]["driver"], "macos-vz");
}

#[test]
fn image_rm_missing_emits_json_diagnostic() {
  let home = tempfile::tempdir().unwrap();
  let output = Command::new(env!("CARGO_BIN_EXE_needy"))
    .env("NEEDY_CONFIG_DIR", home.path().join("config"))
    .env("NEEDY_DATA_DIR", home.path().join("data"))
    .env_remove("NEEDY_EXECUTION_CONTEXT")
    .args([
      "--diagnostic-format",
      "json",
      "image",
      "rm",
      "--driver",
      "macos-vz",
      "nonexistent-image",
    ])
    .output()
    .unwrap();
  assert_eq!(output.status.code(), Some(1));
  assert!(output.stdout.is_empty());
  let diagnostic: serde_json::Value = serde_json::from_slice(&output.stderr).expect("valid json diagnostic");
  assert_eq!(diagnostic["reason"], "image.not_found");
  assert_eq!(diagnostic["facts"]["driver"], "macos-vz");
  assert_eq!(diagnostic["facts"]["image"], "nonexistent-image");
}
