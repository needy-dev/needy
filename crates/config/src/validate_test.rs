use crate::{Source, profile, validate};

fn findings(yaml: &str) -> Vec<String> {
  let p = profile::load(&Source::text(yaml, "/base")).unwrap();
  validate::profile(&p).iter().map(ToString::to_string).collect()
}

#[test]
fn a_consistent_profile_has_no_findings() {
  let found = findings(
    r#"
default_executor: build
unknown: build
executors:
  build: { runtime: docker }
presets:
  mac: { to: build, use: [env] }
  env: { env: { set: { CI: "1" } } }
commands:
  - { match: "uname **", to: actor }
  - { match: "git **", to: executor, executor: build }
  - { match: "make **", to: executor }
  - { match: "xcodebuild **", use: [mac], files: [workspace] }
files:
  workspace: { mode: mount, from: "executor:build:~/workspace/", to: "actor:~/workspace/" }
session_files: [workspace]
"#,
  );

  assert!(found.is_empty(), "{found:?}");
}

#[test]
fn unknown_presets_are_errors() {
  let found = findings("presets: { a: { use: [b] } }\ncommands: [{ match: \"git **\", use: [a, nope] }]");

  assert_eq!(
    found,
    [
      "error: presets.a uses unknown preset \"b\"",
      "error: commands[0] (git **) uses unknown preset \"nope\"",
    ]
  );
}

#[test]
fn unknown_executors_are_errors() {
  let found = findings(
    r#"
default_executor: gone
unknown: lost
executors: { build: {} }
presets: { far: { to: mac } }
commands:
  - { match: "git **", to: biuld }
  - { match: "make **", executor: missing }
files:
  ws: { mode: mount, from: "executor:nowhere:/w", to: "actor:/w" }
"#,
  );

  assert_eq!(
    found,
    [
      "error: default_executor names unknown executor \"gone\"",
      "error: unknown names unknown executor \"lost\"",
      "error: presets.far sends to unknown executor \"mac\"",
      "error: commands[0] (git **) sends to unknown executor \"biuld\"",
      "error: commands[1] (make **) sends to unknown executor \"missing\"",
      "error: files.ws.from names unknown executor \"nowhere\"",
    ]
  );
}

#[test]
fn to_executor_without_a_default_is_an_error() {
  let found = findings("executors: { build: {} }\ncommands: [{ match: \"git **\", to: executor }]");

  assert_eq!(found, ["error: commands[0] (git **) sends to `executor`, but there is no default_executor"]);
}

#[test]
fn unknown_file_mappings_are_errors() {
  let found = findings("session_files: [workspace]\ncommands: [{ match: \"git **\", files: [cache] }]");

  assert_eq!(
    found,
    [
      "error: session_files names unknown file mapping \"workspace\"",
      "error: commands[0] (git **) adds unknown file mapping \"cache\"",
    ]
  );
}
