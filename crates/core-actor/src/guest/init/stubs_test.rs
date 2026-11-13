use std::os::unix::fs::MetadataExt;

use super::*;

// ROOT CAUSE:
//
// If a routed command the actor lacks ran while init received the route
// list again (once in its configuration, once more on the guest control
// stream at start), the program's exec failed with `Text file busy` (126,
// seen by the e2e harness on linux-firecracker), because init rewrote every
// VM stub in place, opening it for writing while the program executed it.
//
// Before the fix, each route list truncated and rewrote every stub.
// The fix leaves a stub that exists alone and makes a new one under a
// temporary name, renamed into place once written, so an executed stub is
// never open for writing.
#[test]
fn a_stub_that_exists_is_left_alone_when_the_route_list_comes_again() {
  let dir = tempfile::tempdir().unwrap();
  let names = vec!["needy-test-routed".to_string()];

  create_in(dir.path(), &names, false);
  let first = std::fs::metadata(dir.path().join("needy-test-routed")).unwrap();
  create_in(dir.path(), &names, false);
  let second = std::fs::metadata(dir.path().join("needy-test-routed")).unwrap();

  assert_eq!(first.ino(), second.ino());
  assert_eq!(first.mtime_nsec(), second.mtime_nsec());
  assert_eq!(second.mode() & 0o777, 0o755);
  let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
  assert_eq!(left, ["needy-test-routed"], "no temporary file is left");
}

#[test]
fn a_container_stub_links_to_the_proxy() {
  let dir = tempfile::tempdir().unwrap();

  create_in(dir.path(), &["needy-test-routed".to_string()], true);
  create_in(dir.path(), &["needy-test-routed".to_string()], true);

  assert_eq!(std::fs::read_link(dir.path().join("needy-test-routed")).unwrap(), Path::new(PROXY));
}
