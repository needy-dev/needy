use super::expand_home;

#[test]
fn a_leading_tilde_is_the_home_directory() {
  assert_eq!(expand_home("~", "/Users/user"), "/Users/user");
  assert_eq!(expand_home("~/project", "/Users/user"), "/Users/user/project");
  assert_eq!(expand_home("~/project", "/Users/user/"), "/Users/user/project");
}

// ROOT CAUSE:
//
// If a native executor's `workspace` had a `~` that was not its first
// character, the driver replaced it with the home directory, because it
// replaced the first `~` anywhere in the path.
//
// Before the fix, `/srv/a~b` became `/srv/a/home/userb`.
// The fix expands only a leading `~` alone or before a `/`.
#[test]
fn a_tilde_anywhere_else_stays() {
  assert_eq!(expand_home("/srv/a~b", "/Users/user"), "/srv/a~b");
  assert_eq!(expand_home("~other/x", "/Users/user"), "~other/x");
  assert_eq!(expand_home("relative", "/Users/user"), "relative");
}

#[test]
fn an_unknown_home_leaves_the_path_as_written() {
  assert_eq!(expand_home("~/project", ""), "~/project");
}
