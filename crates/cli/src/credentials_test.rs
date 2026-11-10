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
