use super::*;
use crate::{Source, profile, validate};

fn load(yaml: &str) -> Result<profile::Profile, crate::Error> {
  profile::load(&Source::text(yaml, "/base"))
}

fn findings(yaml: &str) -> Vec<String> {
  validate::profile(&load(yaml).unwrap()).iter().map(ToString::to_string).collect()
}

#[test]
fn a_sync_mapping_loads_with_its_on_conflict_choices_in_order() {
  let p = load(
    r#"
actor: { driver: linux-firecracker }
files:
  pi:
    mode: sync
    from: "host:/stores/pi"
    to: "actor:~/.pi/agent"
    filter: ["- cache/"]
    on-conflict:
      /auth.json: run
      "*.json": store
      history.jsonl: lines
"#,
  )
  .unwrap();

  let pi = &p.files.mappings["pi"];
  assert_eq!(pi.mode, FileMode::Sync);
  assert_eq!(pi.from, Side::Host("/stores/pi".into()));
  assert_eq!(pi.to, Side::Actor("/home/agent/.pi/agent".into()));
  assert_eq!(
    pi.on_conflict,
    vec![
      ("/auth.json".to_string(), OnConflict::Run),
      ("*.json".to_string(), OnConflict::Store),
      ("history.jsonl".to_string(), OnConflict::Lines),
    ]
  );
}

#[test]
fn a_mapping_without_on_conflict_has_no_choices() {
  let p = load("actor: { driver: linux-firecracker }\nfiles: { pi: { mode: sync, from: \"host:/s\", to: \"actor:~/.pi\" } }").unwrap();

  assert!(p.files.mappings["pi"].on_conflict.is_empty());
}

#[test]
fn an_unknown_on_conflict_choice_is_an_error_naming_the_choices() {
  let err = load("actor: { driver: linux-firecracker }\nfiles: { pi: { mode: sync, from: \"host:/s\", to: \"actor:~/.pi\", on-conflict: { a: newest } } }")
    .unwrap_err()
    .to_string();

  assert!(err.contains("files.pi.on-conflict"), "{err}");
  assert!(err.contains("`store`, `run`, `lines` or `{ driver: COMMAND }`"), "{err}");
}

#[test]
fn a_merge_driver_loads_from_a_driver_map() {
  let p = load(
    r#"
actor: { driver: linux-firecracker }
files:
  oc:
    mode: sync
    from: "host:/s"
    to: "actor:~/.oc"
    on-conflict:
      "opencode.db*": { driver: needy-merge-sqlite }
      auth.json: run
"#,
  )
  .unwrap();

  assert_eq!(
    p.files.mappings["oc"].on_conflict,
    vec![
      ("opencode.db*".to_string(), OnConflict::Driver("needy-merge-sqlite".into())),
      ("auth.json".to_string(), OnConflict::Run),
    ]
  );
}

#[test]
fn an_empty_or_malformed_merge_driver_is_rejected() {
  for yaml in [
    r#"actor: { driver: linux-firecracker }
files: { oc: { mode: sync, from: "host:/s", to: "actor:~/.oc", on-conflict: { "*.db": { driver: "" } } } }"#,
    r#"actor: { driver: linux-firecracker }
files: { oc: { mode: sync, from: "host:/s", to: "actor:~/.oc", on-conflict: { "*.db": { driver: "   " } } } }"#,
    r#"actor: { driver: linux-firecracker }
files: { oc: { mode: sync, from: "host:/s", to: "actor:~/.oc", on-conflict: { "*.db": { other: x } } } }"#,
    r#"actor: { driver: linux-firecracker }
files: { oc: { mode: sync, from: "host:/s", to: "actor:~/.oc", on-conflict: { "*.db": { driver: merge-db, extra: 1 } } } }"#,
  ] {
    let err = load(yaml).unwrap_err().to_string();
    assert!(err.contains("files.oc.on-conflict"), "{err}");
  }
}

#[test]
fn a_sync_mapping_goes_from_a_host_store_to_the_actor() {
  let found = findings(
    r#"
actor: { driver: linux-firecracker }
executors: { build: {} }
files:
  wrong-from: { mode: sync, from: "executor:build:/s", to: "actor:~/a" }
  wrong-to: { mode: sync, from: "host:/s", to: "host:/t" }
"#,
  );

  assert_eq!(
    found,
    [
      "error: files.wrong-from: a sync mapping goes from its store `host:PATH` to `actor:PATH`",
      "error: files.wrong-to: a sync mapping goes from its store `host:PATH` to `actor:PATH`",
    ]
  );
}

#[test]
fn on_conflict_is_for_sync_mappings_only() {
  let found = findings(
    "actor: { driver: linux-firecracker }\nfiles: { a: { mode: copy-in, from: \"host:/s\", to: \"actor:~/a\", on-conflict: { x: run } } }",
  );

  assert_eq!(found, ["error: files.a: on-conflict is for `mode: sync` mappings"]);
}

#[test]
fn two_mappings_claiming_one_actor_path_are_an_error_when_one_syncs() {
  let found = findings(
    r#"
actor: { driver: linux-firecracker }
files:
  login: { mode: copy-in, from: "host:/a", to: "actor:~/.pi/agent/" }
  pi: { mode: sync, from: "host:/s", to: "actor:~/.pi/agent" }
"#,
  );

  assert_eq!(found, ["error: files.pi and files.login both claim /home/agent/.pi/agent in the actor"]);
}

// ROOT CAUSE:
//
// If a profile defined a `copy-in` or `mirror-out` mapping between `actor:`
// and `executor:` (or without `host:` on the required side),
// `validate::profile` accepted it and `needy-daemon`'s file transfer silently
// skipped it at runtime with no error and no files transferred.
//
// Before the fix, `validate::profile` only checked `mode: sync` sides.
// The fix rejects invalid `copy-in` and `mirror-out` sides during validation
// and directs callers to `mode: mount` for actor-executor sharing.
#[test]
fn copy_in_requires_host_source_and_actor_or_executor_destination() {
  let valid = findings(
    r#"
actor: { driver: linux-firecracker }
executors: { build: {} }
files:
  to-actor: { mode: copy-in, from: "host:/src", to: "actor:~/dst" }
  to-exec: { mode: copy-in, from: "host:/src", to: "executor:build:/dst" }
"#,
  );
  assert!(valid.is_empty(), "{valid:?}");

  let invalid = findings(
    r#"
actor: { driver: linux-firecracker }
executors: { build: {} }
files:
  exec-to-actor: { mode: copy-in, from: "executor:build:/src", to: "actor:~/dst" }
  actor-to-exec: { mode: copy-in, from: "actor:~/src", to: "executor:build:/dst" }
  host-to-host: { mode: copy-in, from: "host:/src", to: "host:/dst" }
"#,
  );
  assert_eq!(
    invalid,
    [
      "error: files.actor-to-exec: a copy-in mapping goes from `host:PATH` to `actor:PATH` or `executor:NAME:PATH` (use `mode: mount` to share files between an actor and an executor)",
      "error: files.exec-to-actor: a copy-in mapping goes from `host:PATH` to `actor:PATH` or `executor:NAME:PATH` (use `mode: mount` to share files between an actor and an executor)",
      "error: files.host-to-host: a copy-in mapping goes from `host:PATH` to `actor:PATH` or `executor:NAME:PATH` (use `mode: mount` to share files between an actor and an executor)",
    ]
  );
}

#[test]
fn mirror_out_requires_actor_or_executor_source_and_host_destination() {
  let valid = findings(
    r#"
actor: { driver: linux-firecracker }
executors: { build: {} }
files:
  from-actor: { mode: mirror-out, from: "actor:~/out", to: "host:/dst" }
  from-exec: { mode: mirror-out, from: "executor:build:/out", to: "host:/dst" }
"#,
  );
  assert!(valid.is_empty(), "{valid:?}");

  let invalid = findings(
    r#"
actor: { driver: linux-firecracker }
executors: { build: {} }
files:
  actor-to-exec: { mode: mirror-out, from: "actor:~/out", to: "executor:build:/dst" }
  exec-to-actor: { mode: mirror-out, from: "executor:build:/out", to: "actor:~/dst" }
  host-to-host: { mode: mirror-out, from: "host:/out", to: "host:/dst" }
"#,
  );
  assert_eq!(
    invalid,
    [
      "error: files.actor-to-exec: a mirror-out mapping goes from `actor:PATH` or `executor:NAME:PATH` to `host:PATH` (use `mode: mount` to share files between an actor and an executor)",
      "error: files.exec-to-actor: a mirror-out mapping goes from `actor:PATH` or `executor:NAME:PATH` to `host:PATH` (use `mode: mount` to share files between an actor and an executor)",
      "error: files.host-to-host: a mirror-out mapping goes from `actor:PATH` or `executor:NAME:PATH` to `host:PATH` (use `mode: mount` to share files between an actor and an executor)",
    ]
  );
}
