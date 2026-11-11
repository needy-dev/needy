use crate::{Source, profile, validate};

fn findings(yaml: &str) -> Vec<String> {
  let p = profile::load(&Source::text(yaml, "/base")).unwrap();
  validate::profile(&p).iter().map(ToString::to_string).collect()
}

#[test]
fn a_consistent_profile_has_no_findings() {
  let found = findings(
    r#"
actor: { runtime: firecracker }
default_executor: build
unknown: build
executors:
  build: { provider: docker }
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
run_files: [workspace]
"#,
  );

  assert!(found.is_empty(), "{found:?}");
}

#[test]
fn unknown_presets_are_errors() {
  let found = findings("actor: { runtime: firecracker }\npresets: { a: { use: [b] } }\ncommands: [{ match: \"git **\", use: [a, nope] }]");

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
actor: { runtime: firecracker }
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
  let found = findings("actor: { runtime: firecracker }\nexecutors: { build: {} }\ncommands: [{ match: \"git **\", to: executor }]");

  assert_eq!(found, ["error: commands[0] (git **) sends to `executor`, but there is no default_executor"]);
}

#[test]
fn unknown_file_mappings_are_errors() {
  let found = findings("actor: { runtime: firecracker }\nrun_files: [workspace]\ncommands: [{ match: \"git **\", files: [cache] }]");

  assert_eq!(
    found,
    [
      "error: run_files names unknown file mapping \"workspace\"",
      "error: commands[0] (git **) adds unknown file mapping \"cache\"",
    ]
  );
}

// Before providers had to be named, an executor with `docker:` options was a
// Docker one by default; now the default is `native`, which would ignore
// them silently.
#[test]
fn docker_options_on_a_native_executor_are_an_error() {
  let found = findings("actor: { runtime: firecracker }\nexecutors: { build: { docker: { image: debian } } }");

  assert_eq!(found, ["error: executors.build has `docker:` options but its provider is native; add `provider: docker`"]);
}

#[test]
fn a_container_actor_with_its_oci_runtime_is_valid() {
  let found = findings("actor: { runtime: container, container: { oci_runtime: runsc } }");

  assert!(found.is_empty(), "{found:?}");
}

#[test]
fn a_container_actor_must_name_its_oci_runtime() {
  let found = findings("actor: { runtime: container }");

  assert_eq!(found, ["error: actor.container: a container actor needs `container: { oci_runtime: runc }` (or `runsc`, gVisor)"]);
}

#[test]
fn a_container_actor_whose_options_name_no_oci_runtime_is_an_error() {
  let found = findings("actor: { runtime: container, container: { image: needy-actor:dev } }");

  assert_eq!(found, ["error: actor.container: a container actor needs `container: { oci_runtime: runc }` (or `runsc`, gVisor)"]);
}

#[test]
fn an_apple_container_actor_needs_no_options() {
  let found = findings("actor: { runtime: apple-container }");

  assert!(found.is_empty(), "{found:?}");
}

#[test]
fn an_apple_container_actor_takes_the_image_option() {
  let found = findings("actor: { runtime: apple-container, container: { image: needy-actor:dev } }");

  assert!(found.is_empty(), "{found:?}");
}

// Apple's `container` runs each container in its own VM with its own
// runtime; there is no OCI runtime to choose.
#[test]
fn an_oci_runtime_on_an_apple_container_actor_is_an_error() {
  let found = findings("actor: { runtime: apple-container, container: { oci_runtime: runsc } }");

  assert_eq!(
    found,
    [
      "error: actor.container.oci_runtime is for `runtime: container`; Apple's `container` runs each actor in its own VM and has no OCI runtime to choose"
    ]
  );
}

#[test]
fn container_options_on_a_firecracker_actor_are_an_error() {
  let found = findings("actor: { runtime: firecracker, container: { oci_runtime: runc } }");

  assert_eq!(found, ["error: actor.container is for `runtime: container` or `apple-container`; this actor's runtime is firecracker"]);
}

#[test]
fn container_options_on_a_vz_actor_are_an_error() {
  let found = findings("actor: { runtime: vz, container: { oci_runtime: runc } }");

  assert_eq!(found, ["error: actor.container is for `runtime: container` or `apple-container`; this actor's runtime is vz"]);
}
