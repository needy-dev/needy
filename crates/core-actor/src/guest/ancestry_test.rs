use std::path::Path;

use super::*;

/// Writes a process to a `/proc`-like directory: its `stat` with the parent
/// PID, and its NUL-separated `cmdline`.
fn process(root: &Path, pid: i32, comm: &str, ppid: i32, argv: &[&str]) {
  let dir = root.join(pid.to_string());
  std::fs::create_dir_all(&dir).unwrap();
  // The fields after the parent PID are zero; the reader needs only the PID.
  let rest = vec!["0"; 34].join(" ");
  std::fs::write(dir.join("stat"), format!("{pid} ({comm}) S {ppid} {rest}\n")).unwrap();
  let cmdline: String = argv.iter().map(|a| format!("{a}\0")).collect();
  std::fs::write(dir.join("cmdline"), cmdline).unwrap();
}

#[test]
fn ancestors_are_listed_nearest_first_up_to_init() {
  let root = tempfile::tempdir().unwrap();
  process(root.path(), 1, "init", 0, &["/.needy/init"]);
  process(root.path(), 40, "opencode", 1, &["opencode", "run", "fix it"]);
  // A command name with a space and a parenthesis must not shift the fields.
  process(root.path(), 52, "sh (x)", 40, &["/bin/sh", "-c", "git status"]);
  process(root.path(), 60, "git", 52, &["git", "status"]);

  let found = ancestors(root.path(), 60);

  assert_eq!(
    found,
    [
      vec!["/bin/sh", "-c", "git status"],
      vec!["opencode", "run", "fix it"],
    ]
  );
}

#[test]
fn a_parent_that_exited_ends_the_chain() {
  let root = tempfile::tempdir().unwrap();
  process(root.path(), 60, "git", 52, &["git", "status"]);

  assert!(ancestors(root.path(), 60).is_empty());
}
