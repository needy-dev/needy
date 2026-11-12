use super::*;

fn credential(token: &str) -> Credential {
  Credential {
    client_id: "c1".into(),
    name: "laptop".into(),
    token: token.into(),
    paired_at_ms: 1,
  }
}

#[test]
fn a_stored_token_is_found_by_its_endpoint_with_or_without_a_trailing_slash() {
  let dir = tempfile::tempdir().unwrap();
  let path = dir.path().join("needy").join("credentials.json");

  store(&path, "http://127.0.0.1:7443/", credential("needy-client.c1.a")).unwrap();
  store(&path, "http://127.0.0.1:8443", credential("needy-client.c1.b")).unwrap();

  assert_eq!(find(&path, "http://127.0.0.1:7443").unwrap().unwrap().token, "needy-client.c1.a");
  assert_eq!(find(&path, "http://127.0.0.1:8443/").unwrap().unwrap().token, "needy-client.c1.b");
  assert_eq!(find(&path, "http://127.0.0.1:9443").unwrap(), None);
}

#[test]
fn pairing_again_replaces_the_endpoints_token() {
  let dir = tempfile::tempdir().unwrap();
  let path = dir.path().join("credentials.json");

  store(&path, "unix:/run/needy.sock", credential("needy-client.c1.old")).unwrap();
  store(&path, "unix:/run/needy.sock", credential("needy-client.c1.new")).unwrap();

  assert_eq!(find(&path, "unix:/run/needy.sock").unwrap().unwrap().token, "needy-client.c1.new");
}

#[test]
fn the_endpoint_paired_last_is_the_default() {
  let dir = tempfile::tempdir().unwrap();
  let path = dir.path().join("credentials.json");
  assert_eq!(default_endpoint(&path).unwrap(), None);

  store(&path, "http://127.0.0.1:7443/", credential("needy-client.c1.a")).unwrap();
  assert_eq!(default_endpoint(&path).unwrap().as_deref(), Some("http://127.0.0.1:7443"));
  store(&path, "http://127.0.0.1:8443", credential("needy-client.c1.b")).unwrap();
  assert_eq!(default_endpoint(&path).unwrap().as_deref(), Some("http://127.0.0.1:8443"));
}

#[test]
fn a_file_without_a_default_endpoint_is_read() {
  let dir = tempfile::tempdir().unwrap();
  let path = dir.path().join("credentials.json");
  std::fs::write(&path, r#"{"version":1,"endpoints":{}}"#).unwrap();

  assert_eq!(default_endpoint(&path).unwrap(), None);
}

#[test]
fn no_file_holds_no_token() {
  let dir = tempfile::tempdir().unwrap();

  assert_eq!(find(&dir.path().join("credentials.json"), "http://127.0.0.1:7443").unwrap(), None);
}

#[test]
fn a_file_of_another_version_is_an_error_and_left_alone() {
  let dir = tempfile::tempdir().unwrap();
  let path = dir.path().join("credentials.json");
  std::fs::write(&path, r#"{"version":2,"endpoints":{}}"#).unwrap();

  let error = store(&path, "http://127.0.0.1:7443", credential("t")).unwrap_err();

  assert!(format!("{error:#}").contains("version 2"), "{error:#}");
  assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"version":2,"endpoints":{}}"#);
}

#[cfg(unix)]
#[test]
fn the_file_and_its_directory_are_private() {
  use std::os::unix::fs::PermissionsExt as _;
  let dir = tempfile::tempdir().unwrap();
  let config = dir.path().join("config").join("needy");
  let path = config.join("credentials.json");

  store(&path, "http://127.0.0.1:7443", credential("t")).unwrap();

  let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
  assert_eq!(mode(&path), 0o600);
  assert_eq!(mode(&config), 0o700);
  assert!(!config.join("credentials.json.tmp").exists());
}

// ROOT CAUSE:
//
// If the config directory existed already with mode 0755, `pair`
// wrote `credentials.json` 0600 into it and said nothing, because a
// directory that exists keeps its mode (it may be a shared one).
//
// Before the fix, the directory stayed open to other users silently.
// The fix keeps its mode, as the daemon does with its state directories,
// and warns, naming the directory and its mode.
#[cfg(unix)]
#[test]
fn an_existing_directory_open_to_others_is_kept_and_warned_about() {
  use std::os::unix::fs::PermissionsExt as _;
  let dir = tempfile::tempdir().unwrap();
  let config = dir.path().join("needy");
  std::fs::create_dir(&config).unwrap();
  std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o755)).unwrap();
  let path = config.join("credentials.json");

  let warning = store(&path, "http://127.0.0.1:7443", credential("t")).unwrap();

  let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
  assert_eq!(mode(&path), 0o600);
  assert_eq!(mode(&config), 0o755);
  let warning = warning.expect("a warning about the directory");
  assert!(warning.contains(&format!("{} is open to other users (mode 755)", config.display())), "{warning}");

  let private = dir.path().join("private");
  assert_eq!(store(&private.join("credentials.json"), "http://127.0.0.1:7443", credential("t")).unwrap(), None);
}
