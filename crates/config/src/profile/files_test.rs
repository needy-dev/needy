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
  assert!(err.contains("`store`, `run` or `lines`"), "{err}");
}

// TODO(merge-drivers): this refusal goes with slice M2 of
// 2026-11-17-images-and-run-data-spec.md, which runs merge drivers.
#[test]
fn a_merge_driver_is_refused_until_drivers_exist() {
  let err = load(
    "actor: { driver: linux-firecracker }\nfiles: { oc: { mode: sync, from: \"host:/s\", to: \"actor:~/.oc\", on-conflict: { \"*.db\": { driver: merge-sqlite } } } }",
  )
  .unwrap_err()
  .to_string();

  assert!(err.contains("files.oc.on-conflict"), "{err}");
  assert!(err.contains("merge drivers are not supported yet"), "{err}");
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
