use std::path::{Path, PathBuf};

use super::*;

/// A seed, a run copy, a store and an aside directory under one temporary
/// directory.
struct Fixture {
  _dir: tempfile::TempDir,
  seed: PathBuf,
  run: PathBuf,
  store: PathBuf,
  aside: PathBuf,
}

impl Fixture {
  fn new() -> Self {
    let dir = tempfile::tempdir().unwrap();
    let [seed, run, store, aside] = ["seed", "run", "store", "aside"].map(|name| dir.path().join(name));
    for tree in [&seed, &run, &store] {
      std::fs::create_dir_all(tree).unwrap();
    }
    Self {
      _dir: dir,
      seed,
      run,
      store,
      aside,
    }
  }

  /// Writes `text` at `path` in the seed and, as before the run, in the
  /// run's copy and the store.
  fn seeded(&self, path: &str, text: &str) {
    for tree in [&self.seed, &self.run, &self.store] {
      write(tree, path, text);
    }
  }

  fn merge(&self, policy: &Policy) -> Outcome {
    let trees = Trees {
      seed: &self.seed,
      run: &self.run,
      store: &self.store,
    };
    merge(trees, &self.aside, policy).unwrap()
  }
}

fn write(tree: &Path, path: &str, text: &str) {
  let path = tree.join(path);
  std::fs::create_dir_all(path.parent().unwrap()).unwrap();
  std::fs::write(path, text).unwrap();
}

fn read(tree: &Path, path: &str) -> Option<String> {
  std::fs::read_to_string(tree.join(path)).ok()
}

fn change(path: &str, action: Action) -> Change {
  Change {
    path: path.into(),
    action,
  }
}

fn on_conflict(pattern: &str, choice: Choice) -> Policy {
  Policy {
    on_conflict: vec![(pattern.into(), choice)],
    ..Policy::default()
  }
}

#[test]
fn a_file_only_the_run_changed_is_taken() {
  let f = Fixture::new();
  f.seeded("auth.json", "old");
  write(&f.run, "auth.json", "refreshed");

  let outcome = f.merge(&Policy::default());

  assert_eq!(read(&f.store, "auth.json").as_deref(), Some("refreshed"));
  assert_eq!(outcome.changes, vec![change("auth.json", Action::Taken)]);
}

// ROOT CAUSE:
//
// If two runs of one agent share a store, the run that never touched
// `auth.json` wrote its stale copy back over the one the other run had just
// refreshed, because a mirror-out copies every file back.
//
// The merge keeps the store's copy of every entry the run left as its seed
// had it.
#[test]
fn a_file_the_run_left_alone_keeps_the_stores_newer_copy() {
  let f = Fixture::new();
  f.seeded("auth.json", "old");
  write(&f.store, "auth.json", "refreshed by another run");

  let outcome = f.merge(&Policy::default());

  assert_eq!(read(&f.store, "auth.json").as_deref(), Some("refreshed by another run"));
  assert_eq!(outcome.changes, vec![]);
}

#[test]
fn files_the_run_added_are_taken_with_their_directories_and_the_store_is_created() {
  let f = Fixture::new();
  std::fs::remove_dir(&f.store).unwrap();
  write(&f.run, "sessions/2026/s1.jsonl", "one");

  let outcome = f.merge(&Policy::default());

  assert_eq!(read(&f.store, "sessions/2026/s1.jsonl").as_deref(), Some("one"));
  assert_eq!(
    outcome.changes,
    vec![
      change("sessions", Action::Taken),
      change("sessions/2026", Action::Taken),
      change("sessions/2026/s1.jsonl", Action::Taken),
    ]
  );
}

#[test]
fn a_missing_seed_is_an_empty_tree() {
  let f = Fixture::new();
  std::fs::remove_dir(&f.seed).unwrap();
  write(&f.run, "a", "run");
  write(&f.store, "b", "store");

  f.merge(&Policy::default());

  assert_eq!(read(&f.store, "a").as_deref(), Some("run"));
  assert_eq!(read(&f.store, "b").as_deref(), Some("store"));
}

#[test]
fn files_the_run_removed_are_removed_and_their_emptied_directories_too() {
  let f = Fixture::new();
  f.seeded("old/session.jsonl", "x");
  f.seeded("keep", "k");
  std::fs::remove_dir_all(f.run.join("old")).unwrap();

  let outcome = f.merge(&Policy::default());

  assert!(!f.store.join("old").exists());
  assert_eq!(read(&f.store, "keep").as_deref(), Some("k"));
  assert_eq!(
    outcome.changes,
    vec![
      change("old", Action::Removed),
      change("old/session.jsonl", Action::Removed)
    ]
  );
}

#[test]
fn a_directory_the_run_removed_stays_when_the_store_added_to_it() {
  let f = Fixture::new();
  f.seeded("dir/a", "x");
  std::fs::remove_dir_all(f.run.join("dir")).unwrap();
  write(&f.store, "dir/b", "from another run");

  f.merge(&Policy::default());

  assert!(!f.store.join("dir/a").exists());
  assert_eq!(read(&f.store, "dir/b").as_deref(), Some("from another run"));
}

#[test]
fn a_file_both_changed_keeps_the_stores_and_sets_the_runs_aside_as_a_conflict() {
  let f = Fixture::new();
  f.seeded("installation_id", "seed");
  write(&f.run, "installation_id", "run");
  write(&f.store, "installation_id", "store");

  let outcome = f.merge(&Policy::default());

  assert_eq!(read(&f.store, "installation_id").as_deref(), Some("store"));
  assert_eq!(read(&f.aside, "installation_id").as_deref(), Some("run"));
  assert_eq!(outcome.changes, vec![change("installation_id", Action::Conflict)]);
  assert_eq!(outcome.conflicts().collect::<Vec<_>>(), vec!["installation_id"]);
}

#[test]
fn a_file_both_changed_alike_is_no_conflict() {
  let f = Fixture::new();
  f.seeded("a", "seed");
  write(&f.run, "a", "same");
  write(&f.store, "a", "same");

  assert_eq!(f.merge(&Policy::default()).changes, vec![]);
  assert!(!f.aside.exists());
}

#[test]
fn a_file_the_run_changed_and_the_store_removed_is_a_conflict() {
  let f = Fixture::new();
  f.seeded("a", "seed");
  write(&f.run, "a", "run");
  std::fs::remove_file(f.store.join("a")).unwrap();

  let outcome = f.merge(&Policy::default());

  assert!(!f.store.join("a").exists());
  assert_eq!(read(&f.aside, "a").as_deref(), Some("run"));
  assert_eq!(outcome.changes, vec![change("a", Action::Conflict)]);
}

#[test]
fn on_conflict_store_keeps_the_stores_copy_without_a_conflict() {
  let f = Fixture::new();
  f.seeded("installation_id", "seed");
  write(&f.run, "installation_id", "run");
  write(&f.store, "installation_id", "store");

  let outcome = f.merge(&on_conflict("/installation_id", Choice::Store));

  assert_eq!(read(&f.store, "installation_id").as_deref(), Some("store"));
  assert!(!f.aside.exists());
  assert_eq!(outcome.changes, vec![change("installation_id", Action::Resolved(Choice::Store))]);
}

#[test]
fn on_conflict_run_takes_the_runs_copy() {
  let f = Fixture::new();
  f.seeded("models_cache.json", "seed");
  write(&f.run, "models_cache.json", "run");
  write(&f.store, "models_cache.json", "store");

  let outcome = f.merge(&on_conflict("*.json", Choice::Run));

  assert_eq!(read(&f.store, "models_cache.json").as_deref(), Some("run"));
  assert_eq!(outcome.changes, vec![change("models_cache.json", Action::Resolved(Choice::Run))]);
}

#[test]
fn on_conflict_run_takes_the_runs_removal() {
  let f = Fixture::new();
  f.seeded("a", "seed");
  std::fs::remove_file(f.run.join("a")).unwrap();
  write(&f.store, "a", "store");

  let outcome = f.merge(&on_conflict("a", Choice::Run));

  assert!(!f.store.join("a").exists());
  assert_eq!(outcome.changes, vec![change("a", Action::Resolved(Choice::Run))]);
}

#[test]
fn on_conflict_lines_appends_what_the_run_appended_once() {
  let f = Fixture::new();
  f.seeded("history.jsonl", "a\n");
  write(&f.run, "history.jsonl", "a\nfrom run\n");
  write(&f.store, "history.jsonl", "a\nfrom another run\n");
  let policy = on_conflict("history.jsonl", Choice::Lines);

  let outcome = f.merge(&policy);
  // A merge interrupted after its append runs again when it is retried.
  let again = f.merge(&policy);

  assert_eq!(read(&f.store, "history.jsonl").as_deref(), Some("a\nfrom another run\nfrom run\n"));
  assert_eq!(outcome.changes, vec![change("history.jsonl", Action::Resolved(Choice::Lines))]);
  assert_eq!(again.changes, vec![change("history.jsonl", Action::Resolved(Choice::Lines))]);
}

#[test]
fn on_conflict_lines_starts_appended_lines_on_a_line_of_their_own() {
  let f = Fixture::new();
  f.seeded("log", "a\n");
  write(&f.run, "log", "a\nb\n");
  write(&f.store, "log", "a\nunterminated");

  f.merge(&on_conflict("log", Choice::Lines));

  assert_eq!(read(&f.store, "log").as_deref(), Some("a\nunterminated\nb\n"));
}

#[test]
fn on_conflict_lines_is_a_conflict_when_the_run_did_more_than_append() {
  let f = Fixture::new();
  f.seeded("history.jsonl", "a\nb\n");
  write(&f.run, "history.jsonl", "b\nc\n");
  write(&f.store, "history.jsonl", "a\nb\nd\n");

  let outcome = f.merge(&on_conflict("history.jsonl", Choice::Lines));

  assert_eq!(read(&f.store, "history.jsonl").as_deref(), Some("a\nb\nd\n"));
  assert_eq!(read(&f.aside, "history.jsonl").as_deref(), Some("b\nc\n"));
  assert_eq!(outcome.changes, vec![change("history.jsonl", Action::Conflict)]);
}

#[test]
fn the_first_matching_on_conflict_pattern_applies() {
  let f = Fixture::new();
  for name in ["auth.json", "other.json"] {
    f.seeded(name, "seed");
    write(&f.run, name, "run");
    write(&f.store, name, "store");
  }
  let policy = Policy {
    on_conflict: vec![
      ("/auth.json".into(), Choice::Run),
      ("*.json".into(), Choice::Store),
    ],
    ..Policy::default()
  };

  f.merge(&policy);

  assert_eq!(read(&f.store, "auth.json").as_deref(), Some("run"));
  assert_eq!(read(&f.store, "other.json").as_deref(), Some("store"));
}

#[test]
fn entries_the_filter_excludes_are_not_merged() {
  let f = Fixture::new();
  write(&f.run, "cache/blob", "x");
  write(&f.run, "debug.log", "x");
  write(&f.run, "session.jsonl", "s");
  let policy = Policy {
    filter: vec!["- cache/".into(), "- *.log".into()],
    ..Policy::default()
  };

  let outcome = f.merge(&policy);

  assert!(!f.store.join("cache").exists());
  assert!(!f.store.join("debug.log").exists());
  assert_eq!(outcome.changes, vec![change("session.jsonl", Action::Taken)]);
}

#[test]
fn a_file_the_run_turned_into_a_directory_is_taken() {
  let f = Fixture::new();
  f.seeded("state", "file");
  std::fs::remove_file(f.run.join("state")).unwrap();
  write(&f.run, "state/inner", "x");

  f.merge(&Policy::default());

  assert_eq!(read(&f.store, "state/inner").as_deref(), Some("x"));
}

#[test]
fn a_run_file_under_a_path_the_store_made_a_file_is_set_aside() {
  let f = Fixture::new();
  write(&f.run, "dir/inner", "run");
  write(&f.store, "dir", "store file");

  let outcome = f.merge(&Policy::default());

  assert_eq!(read(&f.store, "dir").as_deref(), Some("store file"));
  assert_eq!(read(&f.aside, "dir/inner").as_deref(), Some("run"));
  assert_eq!(outcome.conflicts().collect::<Vec<_>>(), vec!["dir", "dir/inner"]);
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_is_merged_by_its_target() {
  let f = Fixture::new();
  std::os::unix::fs::symlink("target-a", f.run.join("current")).unwrap();

  let outcome = f.merge(&Policy::default());

  assert_eq!(std::fs::read_link(f.store.join("current")).unwrap(), Path::new("target-a"));
  assert_eq!(outcome.changes, vec![change("current", Action::Taken)]);
}

#[cfg(unix)]
#[test]
fn a_taken_file_keeps_the_runs_permissions() {
  use std::os::unix::fs::PermissionsExt;
  let f = Fixture::new();
  write(&f.run, "bin/tool", "#!/bin/sh\n");
  std::fs::set_permissions(f.run.join("bin/tool"), std::fs::Permissions::from_mode(0o755)).unwrap();

  f.merge(&Policy::default());

  let mode = std::fs::metadata(f.store.join("bin/tool")).unwrap().permissions().mode();
  assert_eq!(mode & 0o777, 0o755);
}

#[test]
fn files_matching_a_driver_rule_are_skipped_and_returned_as_driver_claims() {
  let f = Fixture::new();
  f.seeded("opencode.db", "seed-db");
  write(&f.run, "opencode.db", "run-db");
  write(&f.run, "opencode.db-wal", "run-wal");
  write(&f.store, "opencode.db", "store-db");
  write(&f.store, "opencode.db-shm", "store-shm");
  write(&f.run, "notes.txt", "normal-file");

  let policy = on_conflict("opencode.db*", Choice::Driver("needy-merge-sqlite".into()));
  let outcome = f.merge(&policy);

  // Non-matching files merge normally.
  assert_eq!(read(&f.store, "notes.txt").as_deref(), Some("normal-file"));
  assert_eq!(outcome.changes, vec![change("notes.txt", Action::Taken)]);
  // Claimed files are untouched in store and not put in aside.
  assert_eq!(read(&f.store, "opencode.db").as_deref(), Some("store-db"));
  assert!(!f.store.join("opencode.db-wal").exists());
  assert!(!f.aside.exists());
  // All matching files across all three trees (including side files) are claimed.
  assert_eq!(
    outcome.driver_claims,
    vec![DriverClaim {
      driver: "needy-merge-sqlite".into(),
      paths: vec![
        "opencode.db".into(),
        "opencode.db-shm".into(),
        "opencode.db-wal".into(),
      ],
    }]
  );
}

#[test]
fn driver_claimed_files_untouched_by_the_run_produce_no_claim() {
  let f = Fixture::new();
  f.seeded("opencode.db", "same");
  write(&f.store, "opencode.db", "store-updated");

  let policy = on_conflict("opencode.db*", Choice::Driver("needy-merge-sqlite".into()));
  let outcome = f.merge(&policy);

  assert_eq!(read(&f.store, "opencode.db").as_deref(), Some("store-updated"));
  assert!(outcome.driver_claims.is_empty());
}

#[test]
fn driver_staging_and_applying_replaces_claimed_files_and_cleans_stale_side_files() {
  let f = Fixture::new();
  f.seeded("opencode.db", "seed-db");
  write(&f.run, "opencode.db", "run-db");
  write(&f.run, "opencode.db-wal", "run-wal");
  write(&f.store, "opencode.db", "store-db");
  write(&f.store, "opencode.db-wal", "store-wal");
  write(&f.store, "other.txt", "untouched");

  let claimed = vec!["opencode.db".to_string(), "opencode.db-wal".to_string()];
  let staging = tempfile::tempdir().unwrap();
  let trees = Trees {
    seed: &f.seed,
    run: &f.run,
    store: &f.store,
  };
  stage_driver_trees(trees, &claimed, staging.path()).unwrap();

  assert_eq!(read(&staging.path().join("seed"), "opencode.db").as_deref(), Some("seed-db"));
  assert_eq!(read(&staging.path().join("run"), "opencode.db-wal").as_deref(), Some("run-wal"));
  assert_eq!(read(&staging.path().join("store"), "opencode.db-wal").as_deref(), Some("store-wal"));
  assert!(!staging.path().join("store/other.txt").exists());

  // Simulate driver writing checkpointed opencode.db (no -wal) into out/.
  let out = staging.path().join("out");
  write(&out, "opencode.db", "merged-db");
  apply_driver_output(&out, &f.store, &claimed).unwrap();

  assert_eq!(read(&f.store, "opencode.db").as_deref(), Some("merged-db"));
  assert!(!f.store.join("opencode.db-wal").exists(), "stale -wal side file must be removed");
  assert_eq!(read(&f.store, "other.txt").as_deref(), Some("untouched"));
}
