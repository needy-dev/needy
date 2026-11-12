use std::path::Path;

use super::*;
use crate::value::Glob;

fn load_text(yaml: &str) -> Result<Profile, crate::Error> {
  load(&Source::text(yaml, "/base"))
}

/// A directory of profile files for `extends` tests.
fn files(entries: &[(&str, &str)]) -> tempfile::TempDir {
  let dir = tempfile::tempdir().unwrap();
  for (name, text) in entries {
    let path = dir.path().join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
  }
  dir
}

fn secs(d: Option<crate::value::Duration>) -> Option<u64> {
  d.map(|d| d.as_secs())
}

const PROFILE: &str = r#"
user: { name: agent, uid: 1000, gid: 1000 }
actor:
  driver: linux-firecracker
  recipe: gce-e2-standard-4
  limits: { single_process: true, self_replace: true }
  command: opencode run hello
  network:
    allow: [openrouter.ai, "*.models.dev"]
    services:
      ollama: { url: "http://127.0.0.1:11434" }
executors:
  build:
    runner: { labels: [kvm, os=linux] }
    driver: linux-container
    container:
      build: { context: ./env, dockerfile: Dockerfile.build, target: toolchain, args: { RUST_VERSION: "1.97" } }
      cpus: 4
      memory: 8GiB
    key: "{run}"
    max_procs: 4
    on_full: wait
    max_instances: 16
    idle_timeout: 10m
  mac:
    runner: mac
    key: "{rule}"
    max_procs: 1
executor_network:
  allow: [registry.npmjs.org]
  record: true
presets:
  clean-env: { env: { clear: true, pass: [PATH, HOME] } }
commands:
  - match: "{node,bun} {script} **"
    when: { captures: { script: "**/pi/dist/cli.js" }, has: [--x] }
    to: actor
    final: true
  - match: "**"
    to: build
    use: [clean-env]
    args: { prepend: [-c, color.ui=never] }
    wrap: timeout 600
    timeout: 1m
resolvers:
  - { match: "mytool run {script} **", effective: "{script} **" }
unknown: build
files:
  skills: { mode: copy-in, from: "host:./.agents/", to: "actor:~/.agents/" }
  codex-home:
    mode: copy-in
    from: "host:~/.codex/"
    to: "actor:~/.codex/"
    filter: ["- /auth.json", "- /sessions/**", "+ **"]
  workspace: { mode: mount, from: "executor:build:~/workspace/", to: "actor:~/workspace/", writable: true }
  sessions: { mode: mirror-out, from: "actor:~/.codex/sessions/", to: "host:./sessions/", when: continuous }
  ignore_file: .needyignore
run_files: [skills, workspace]
chaos:
  seed: 42
  faults:
    flaky-json: { file: { open: "~/workspace/**/*.json" }, errno: EIO, probability: 0.01 }
    slow-npm: { net: { connect: registry.npmjs.org }, delay: 2s, nth: 3 }
    overheat: { sensor: { temp_c: 101 }, at: 5m }
    curl-down: { exec: "curl **", exit: 7, stderr: "curl: (7) refused", times: 3, after: 1m, until: 1h }
"#;

#[test]
fn loads_every_section() {
  let p = load_text(PROFILE).unwrap();

  assert_eq!(p.user.home(), "/home/agent");
  assert_eq!(p.base_dir, Path::new("/base"));
  assert_eq!(p.actor.driver, ActorDriver::LinuxFirecracker);
  assert_eq!(p.actor.recipe.as_deref(), Some("gce-e2-standard-4"));
  assert!(p.actor.limits.single_process);
  assert_eq!(&*p.actor.command, ["opencode", "run", "hello"]);
  assert!(p.actor.network.allows("api.models.dev"));
  assert!(!p.actor.network.allows("example.com"));
  assert!(p.actor.network.allows("API.Models.Dev"));
  assert!(p.actor.network.allows("models.dev"));
  assert_eq!(p.actor.network.services["ollama"].url, "http://127.0.0.1:11434");

  let build = &p.executors["build"];
  assert_eq!(build.driver, ExecutorDriver::LinuxContainer);
  let labels = vec![
    LabelMatch {
      key: "kvm".into(),
      value: None,
    },
    LabelMatch {
      key: "os".into(),
      value: Some("linux".into()),
    },
  ];
  assert_eq!(build.runner, Some(RunnerSelector::Labels(labels)));
  assert_eq!(secs(build.idle_timeout), Some(600));
  assert_eq!(build.max_procs, Some(4));
  let docker = build.container.as_ref().unwrap();
  assert_eq!(docker.cpus.map(|c| c.nanos()), Some(4_000_000_000));
  assert_eq!(docker.memory.map(|m| m.bytes()), Some(8 << 30));
  assert_eq!(docker.build.as_ref().unwrap().args["RUST_VERSION"], "1.97");
  let mac = &p.executors["mac"];
  assert_eq!(mac.driver, ExecutorDriver::Native);
  assert_eq!(mac.runner, Some(RunnerSelector::Name("mac".into())));
  assert_eq!(mac.key.as_str(), "{rule}");
  assert_eq!(mac.on_full, OnFull::Wait);
  assert!(p.executor_network.record);

  let pi = &p.commands[0];
  assert_eq!(pi.matcher, "{node,bun} {script} **");
  assert_eq!(pi.when.captures["script"], Glob::new("**/pi/dist/cli.js"));
  assert_eq!(pi.when.has, ["--x"]);
  assert_eq!(pi.actions.to.as_deref(), Some("actor"));
  assert!(pi.actions.stop);
  let all = &p.commands[1];
  assert_eq!(all.actions.uses, ["clean-env"]);
  assert_eq!(&*all.actions.args.prepend, ["-c", "color.ui=never"]);
  assert_eq!(&*all.actions.wrap, ["timeout", "600"]);
  assert_eq!(secs(all.actions.timeout), Some(60));
  assert!(p.presets["clean-env"].env.clear);
  assert_eq!(p.resolvers[0].effective.as_deref(), Some("{script} **"));
  assert_eq!(p.unknown, UnknownPolicy::Executor("build".into()));

  let files = &p.files;
  assert_eq!(
    files.mappings["workspace"].from,
    Side::Executor {
      name: "build".into(),
      path: "~/workspace/".into()
    }
  );
  assert_eq!(files.mappings["skills"].to, Side::Actor("/home/agent/.agents/".into()));
  assert_eq!(files.mappings["codex-home"].from, Side::Host("~/.codex/".into()));
  assert_eq!(files.mappings["sessions"].when, MirrorWhen::Continuous);
  assert_eq!(files.ignore_file.as_deref(), Some(".needyignore"));
  assert_eq!(p.run_files, ["skills", "workspace"]);

  assert_eq!(p.chaos.seed, 42);
  let faults = &p.chaos.faults;
  assert_eq!(faults.keys().collect::<Vec<_>>(), ["curl-down", "flaky-json", "overheat", "slow-npm"]);
  assert_eq!(faults["flaky-json"].domain(), FaultDomain::File);
  // A file fault's glob matches the path in the actor, `~` expanded.
  assert_eq!(faults["flaky-json"].file.as_ref().map(|f| f.open.as_str()), Some("/home/agent/workspace/**/*.json"));
  assert_eq!(faults["slow-npm"].domain(), FaultDomain::Net);
  assert_eq!(faults["slow-npm"].delay.map(|d| d.as_millis()), Some(2000));
  assert_eq!(faults["overheat"].domain(), FaultDomain::Sensor);
  assert_eq!(faults["overheat"].sensor.as_ref().and_then(|r| r.temp_c), Some(101.0));
  assert_eq!(secs(faults["overheat"].at), Some(300));
  let curl = &faults["curl-down"];
  assert_eq!((curl.domain(), curl.exit, curl.times), (FaultDomain::Exec, Some(7), Some(3)));
  assert_eq!((secs(curl.after), secs(curl.until)), (Some(60), Some(3600)));
}

#[test]
fn a_profile_that_names_only_its_actor_driver_has_the_defaults() {
  let p = load_text("actor: { driver: linux-firecracker }\n").unwrap();

  assert_eq!(p.user.name, "agent");
  assert_eq!((p.user.uid, p.user.gid), (1000, 1000));
  assert!(p.executors.is_empty());
  assert!(p.commands.is_empty());
  assert_eq!(p.unknown, UnknownPolicy::Sticky);
}

#[test]
fn the_actor_image_is_one_field_for_every_driver_with_the_container_base_by_default() {
  let named = load_text("actor: { driver: macos-vz, image: example.com/agent:1 }\n").unwrap();
  let unnamed = load_text("actor: { driver: linux-container, container: { oci_runtime: runc } }\n").unwrap();

  assert_eq!(named.actor.image.as_deref(), Some("example.com/agent:1"));
  assert_eq!(named.actor.container_image(), "example.com/agent:1");
  assert_eq!(unnamed.actor.image, None);
  assert_eq!(unnamed.actor.container_image(), CONTAINER_IMAGE);
}

#[test]
fn the_container_options_no_longer_name_the_image() {
  let err =
    load_text("actor: { driver: linux-container, container: { oci_runtime: runc, image: needy-actor:dev } }\n").unwrap_err().to_string();

  assert!(err.contains("actor.container.image: unknown field `image`"), "{err}");
}

#[test]
fn executor_defaults() {
  let p = load_text("actor: { driver: linux-firecracker }\nexecutors: { build: {} }").unwrap();

  let build = &p.executors["build"];
  assert_eq!(build.runner, None);
  assert_eq!(build.driver, ExecutorDriver::Native);
  assert_eq!(build.key.as_str(), "{run}");
  assert_eq!(build.on_full, OnFull::Wait);
}

#[test]
fn a_file_profile_resolves_against_its_directory() {
  let dir = files(&[("team/profile.yaml", "user: { name: dev }\nactor: { driver: linux-firecracker }")]);

  let p = load(&Source::file(dir.path().join("team/profile.yaml"))).unwrap();

  assert_eq!(p.base_dir, dir.path().join("team"));
  assert_eq!(p.user.home(), "/home/dev");
}

#[test]
fn a_missing_file_names_its_path() {
  let err = load(&Source::file("/nonexistent/needy/profile.yaml")).unwrap_err();

  assert_eq!(err.to_string(), "cannot read profile /nonexistent/needy/profile.yaml");
}

// One merge model: maps merge key by key, a child's scalar or list replaces
// the parent's, and `commands` and `resolvers` put the child's entries first.
#[test]
fn extends_merges_maps_and_replaces_scalars_and_lists() {
  let dir = files(&[
    (
      "base.yaml",
      r#"
default_executor: build
actor:
  driver: linux-firecracker
  command: [sh, -c, "echo base"]
  network: { allow: [a.example.com], services: { model: { url: "http://127.0.0.1:1" } } }
executors:
  build: { driver: linux-container, max_procs: 4, key: "{profile}" }
  mac: { runner: mac }
presets:
  quiet: { env: { set: { QUIET: "1" } } }
commands:
  - { match: "**", env: { set: { BASE: "1" } } }
resolvers:
  - { match: "make **", sticky: true }
run_files: [workspace]
files:
  workspace: { mode: mount, from: "executor:build:~/workspace/", to: "actor:~/workspace/", writable: true }
"#,
    ),
    (
      "child.yaml",
      r#"
extends: base.yaml
actor:
  network: { allow: [b.example.com] }
executors:
  build: { max_procs: 8 }
commands:
  - { match: "git **", to: actor }
resolvers:
  - { match: "mytool **", sticky: true }
run_files: []
files:
  workspace: { writable: false }
"#,
    ),
  ]);

  let p = load(&Source::file(dir.path().join("child.yaml"))).unwrap();

  // Maps merge recursively.
  assert_eq!(&*p.actor.command, ["sh", "-c", "echo base"]);
  assert_eq!(p.actor.network.services["model"].url, "http://127.0.0.1:1");
  let build = &p.executors["build"];
  assert_eq!(build.max_procs, Some(8));
  assert_eq!(build.key.as_str(), "{profile}");
  assert_eq!(p.executors["mac"].runner, Some(RunnerSelector::Name("mac".into())));
  assert_eq!(build.driver, ExecutorDriver::LinuxContainer);
  assert!(p.presets.contains_key("quiet"));
  assert!(!p.files.mappings["workspace"].writable);
  assert_eq!(p.files.mappings["workspace"].mode, FileMode::Mount);
  // Scalars and lists replace.
  assert_eq!(p.default_executor.as_deref(), Some("build"));
  assert_eq!(p.actor.network.allow, [Glob::new("b.example.com")]);
  assert!(p.run_files.is_empty());
  // Prepend lists: the child's entries first.
  let matchers: Vec<&str> = p.commands.iter().map(|r| r.matcher.as_str()).collect();
  assert_eq!(matchers, ["git **", "**"]);
  let resolvers: Vec<&str> = p.resolvers.iter().map(|r| r.matcher.as_str()).collect();
  assert_eq!(resolvers, ["mytool **", "make **"]);
}

#[test]
fn a_null_in_the_child_removes_the_parent_value() {
  let dir = files(&[
    ("base.yaml", "actor: { driver: linux-firecracker }\ndefault_executor: build\n"),
    ("child.yaml", "extends: base.yaml\ndefault_executor: ~\n"),
  ]);

  let p = load(&Source::file(dir.path().join("child.yaml"))).unwrap();

  assert_eq!(p.default_executor, None);
}

#[test]
fn extends_resolves_against_the_extending_file() {
  let dir = files(&[
    ("a/child.yaml", "extends: ../b/middle.yaml\n"),
    ("b/middle.yaml", "extends: base.yaml\nuser: { uid: 1001 }\n"),
    ("b/base.yaml", "user: { name: dev, uid: 1, gid: 7 }\nactor: { driver: linux-firecracker }\n"),
  ]);

  let p = load(&Source::file(dir.path().join("a/child.yaml"))).unwrap();

  assert_eq!((p.user.name.as_str(), p.user.uid, p.user.gid), ("dev", 1001, 7));
  assert_eq!(p.base_dir, dir.path().join("a"));
}

#[test]
fn text_extends_resolves_against_its_base_dir() {
  let dir = files(&[("base.yaml", "user: { name: dev }\nactor: { driver: linux-firecracker }\n")]);

  let p = load(&Source::text("extends: base.yaml\n", dir.path())).unwrap();

  assert_eq!(p.user.name, "dev");
}

#[test]
fn extends_cycles_are_rejected() {
  let dir = files(&[
    ("a.yaml", "extends: b.yaml\n"),
    ("b.yaml", "extends: a.yaml\n"),
  ]);

  let err = load(&Source::file(dir.path().join("a.yaml"))).unwrap_err().to_string();

  assert!(err.contains("cycles back to"), "{err}");
  assert!(err.contains("a.yaml"), "{err}");
}

#[test]
fn extends_depth_is_limited() {
  let mut entries: Vec<(String, String)> = (0..20).map(|i| (format!("p{i}.yaml"), format!("extends: p{}.yaml\n", i + 1))).collect();
  entries.push(("p20.yaml".into(), String::new()));
  let refs: Vec<(&str, &str)> = entries.iter().map(|(n, t)| (n.as_str(), t.as_str())).collect();
  let dir = files(&refs);

  let err = load(&Source::file(dir.path().join("p0.yaml"))).unwrap_err().to_string();

  assert!(err.contains("nests more than 16 levels"), "{err}");
}

#[test]
fn there_are_no_built_in_profiles() {
  let err = load_text("extends: builtin:base-linux\n").unwrap_err().to_string();

  assert!(err.contains("no built-in profile named \"base-linux\""), "{err}");
}

#[test]
fn a_missing_parent_names_its_path() {
  let dir = files(&[("child.yaml", "extends: gone.yaml\n")]);

  let err = load(&Source::file(dir.path().join("child.yaml"))).unwrap_err().to_string();

  assert_eq!(err, format!("cannot read profile {}", dir.path().join("gone.yaml").display()));
}

#[test]
fn unknown_keys_are_rejected_with_their_path() {
  let err = load_text("executors: { build: { runtme: native } }").unwrap_err().to_string();

  assert!(err.starts_with("invalid profile "), "{err}");
  assert!(err.contains("executors.build.runtme: unknown field `runtme`, expected one of"), "{err}");
}

#[test]
fn unknown_rule_keys_are_rejected() {
  let err = load_text("commands: [{ match: \"git **\", too: build }]").unwrap_err().to_string();

  assert!(err.contains("commands[0]: too: unknown field `too`"), "{err}");
}

// `when` used to read every key it did not know as a capture glob, so a typo
// such as `cdw:` silently became a capture that never matched.
#[test]
fn flattened_capture_globs_are_rejected() {
  let err = load_text("commands: [{ match: \"node {script}\", when: { script: \"**/cli.js\" }, to: actor }]").unwrap_err().to_string();

  assert!(err.contains("commands[0].when.script: unknown field `script`"), "{err}");
  assert!(err.contains("`captures`"), "{err}");
}

#[test]
fn a_rule_needs_a_match() {
  let err = load_text("commands: [{ to: actor }]").unwrap_err().to_string();

  assert!(err.contains("commands[0]: missing field `match`"), "{err}");
}

#[test]
fn bad_values_are_rejected() {
  let err = load_text("executors: { build: { idle_timeout: soon } }").unwrap_err().to_string();
  assert!(err.contains("executors.build.idle_timeout: invalid duration \"soon\""), "{err}");

  let err = load_text("executors: { build: { driver: podman } }").unwrap_err().to_string();
  assert!(err.contains("unknown variant `podman`"), "{err}");

  let err = load_text("chaos: { faults: { a: { sensor: { temp: 90 } } } }").unwrap_err().to_string();
  assert!(err.contains("unknown field `temp`"), "{err}");
}

/// Loads a profile whose only fault is `fault`, and returns the error.
fn fault_error(fault: &str) -> String {
  load_text(&format!(
    "actor: {{ driver: linux-firecracker }}
chaos: {{ faults: {{ f: {fault} }} }}
"
  ))
  .unwrap_err()
  .to_string()
}

#[test]
fn a_fault_names_exactly_one_domain() {
  let err = fault_error("{ exit: 7 }");
  assert!(err.contains("chaos.faults.f: a fault names one domain: `exec`, `file`, `net` or `sensor`"), "{err}");
  let err = fault_error("{ exec: \"curl **\", net: { connect: example.com }, exit: 7 }");
  assert!(err.contains("a fault names one domain, not exec and net"), "{err}");
  // The removed `on:` form is an unknown field.
  let err = fault_error("{ on: exec, match: \"curl **\", errno: EIO }");
  assert!(err.contains("unknown field `on`"), "{err}");
}

#[test]
fn each_domain_takes_only_its_own_effects() {
  let err = fault_error("{ exec: \"curl **\", errno: ECONNREFUSED }");
  assert!(err.contains("errno is not for exec faults; an exec fault takes exit, signal, hang or delay"), "{err}");
  let err = fault_error("{ file: { open: \"**\" }, exit: 1 }");
  assert!(err.contains("exit is not for file faults; a file fault takes errno or delay"), "{err}");
  let err = fault_error("{ net: { connect: example.com }, errno: EIO }");
  assert!(err.contains("errno is not for net faults"), "{err}");
  let err = fault_error("{ sensor: { temp_c: 90 }, probability: 0.5 }");
  assert!(err.contains("probability is not for sensor faults; a sensor fault takes only `at`"), "{err}");
  let err = fault_error("{ exec: \"curl **\", exit: 7, at: 1s }");
  assert!(err.contains("at is not for exec faults"), "{err}");
}

#[test]
fn a_fault_needs_an_effect_it_can_deliver() {
  let err = fault_error("{ exec: \"curl **\" }");
  assert!(err.contains("an exec fault needs an effect"), "{err}");
  let err = fault_error("{ exec: \"curl **\", exit: 7, signal: SIGSEGV }");
  assert!(err.contains("ends the command one way"), "{err}");
  let err = fault_error("{ exec: \"curl **\", stderr: oops }");
  assert!(err.contains("stderr is written before exit, signal or hang"), "{err}");
  let err = fault_error("{ exec: \"curl **\", signal: SIGCHLD }");
  assert!(err.contains("signal SIGCHLD does not end a process"), "{err}");
  // Only the errnos sshfs passes on.
  let err = fault_error("{ file: { open: \"**\" }, errno: ECONNRESET }");
  assert!(err.contains("errno ECONNRESET cannot reach the program"), "{err}");
  let err = fault_error("{ net: { connect: example.com }, status: 503 }");
  assert!(err.contains("status answers a plain-HTTP request"), "{err}");
  let err = fault_error("{ net: { connect: example.com }, refuse: true, reset: true }");
  assert!(err.contains("fails the connection one way"), "{err}");
  let err = fault_error("{ net: { connect: a, request: b }, refuse: true }");
  assert!(err.contains("net matches `connect` or `request`, not both"), "{err}");
}

#[test]
fn fault_triggers_are_checked() {
  let err = fault_error("{ exec: \"curl **\", exit: 7, probability: 1.5 }");
  assert!(err.contains("probability 1.5 is not between 0 and 1"), "{err}");
  let err = fault_error("{ exec: \"curl **\", exit: 7, nth: 2, times: 3 }");
  assert!(err.contains("nth fires on one match"), "{err}");
  let err = fault_error("{ exec: \"curl **\", exit: 7, after: 1m, until: 30s }");
  assert!(err.contains("until must be later than after"), "{err}");
}

#[test]
fn a_signal_is_spelled_with_its_sig_prefix() {
  let p = load_text("actor: { driver: linux-firecracker }\nchaos: { faults: { crash: { exec: \"make **\", signal: SEGV } } }\n").unwrap();
  assert_eq!(p.chaos.faults["crash"].signal.as_deref(), Some("SIGSEGV"));
}

#[test]
fn a_fault_written_back_loads_the_same() {
  let p = load_text(PROFILE).unwrap();
  for (name, fault) in &p.chaos.faults {
    let text = serde_yaml::to_string(fault).unwrap();
    let again: Fault = serde_yaml::from_str(&text).unwrap();
    assert_eq!(&again, fault, "{name}: {text}");
  }
}

#[test]
fn file_sides_need_a_prefix() {
  let err = load_text("files: { a: { mode: mount, from: \"nowhere\", to: \"actor:~/a\" } }").unwrap_err().to_string();
  assert!(err.contains("files.a.from: file side must start with host:, actor: or executor:"), "{err}");

  let err = load_text("files: { a: { mode: mount, from: \"executor::x\", to: \"actor:~/a\" } }").unwrap_err().to_string();
  assert!(err.contains("executor side needs a name"), "{err}");
}

#[test]
fn yaml_syntax_errors_name_the_line() {
  let err = load_text("user: {\n").unwrap_err();

  let text = format!("{err}: {}", std::error::Error::source(&err).unwrap());
  assert!(text.starts_with("invalid YAML in profile "), "{text}");
  assert!(text.contains("line"), "{text}");
}

#[test]
fn actor_sides_expand_home_for_the_profile_user_and_executor_sides_keep_it() {
  let p = load_text(
    "user: { name: dev }\nactor: { driver: linux-firecracker }\nfiles: { out: { mode: mirror-out, from: \"executor:b:~\", to: \"host:~/out\" } }",
  )
  .unwrap();

  assert_eq!(
    p.files.mappings["out"].from,
    Side::Executor {
      name: "b".into(),
      // The executor's home, which the daemon knows once the instance
      // exists.
      path: "~".into()
    }
  );
  assert_eq!(p.files.mappings["out"].to, Side::Host("~/out".into()));
  assert_eq!(p.user.expand_home("~/w"), "/home/dev/w");
  assert_eq!(p.user.expand_home("./w"), "./w");
}

#[test]
fn trace_defaults_to_sqlite_and_jsonl_without_timing() {
  let p = load_text("actor: { driver: linux-firecracker }\n").unwrap();

  assert_eq!(p.trace.sinks, [TraceSink::Sqlite, TraceSink::Jsonl]);
  assert!(!p.trace.timing);
}

#[test]
fn trace_selects_sinks_and_timing() {
  let p = load_text("actor: { driver: linux-firecracker }\ntrace: { sinks: [jsonl], timing: true }\n").unwrap();

  assert_eq!(p.trace.sinks, [TraceSink::Jsonl]);
  assert!(p.trace.timing);
  assert!(load_text("actor: { driver: linux-firecracker }\ntrace: { sinks: [] }\n").unwrap().trace.sinks.is_empty());
  let err = load_text("trace: { sinks: [otel] }\n").unwrap_err().to_string();
  assert!(err.contains("trace.sinks[0]: unknown variant `otel`, expected `sqlite` or `jsonl`"), "{err}");
}

#[test]
fn a_profile_is_named_after_its_file() {
  assert_eq!(name(Path::new("/home/user/review.profile.yaml")), "review");
  assert_eq!(name(Path::new("/home/user/review.yml")), "review");
  assert_eq!(name(Path::new("/home/user/examples/bench/mac.profile.yaml")), "mac");
}

#[test]
fn a_file_named_profile_takes_its_directory_name() {
  assert_eq!(name(Path::new("/home/user/examples/smoke/profile.yaml")), "smoke");
}

#[test]
fn a_resolved_profile_merges_its_parents_and_makes_host_paths_absolute() {
  let dir = files(&[
    (
      "base.yaml",
      "actor: { driver: linux-firecracker }\nexecutors:\n  here: { driver: native }\n  box: { driver: linux-container, container: { mounts: [{ type: bind, source: cache, target: /c }, { type: volume, source: v, target: /v }] } }\n",
    ),
    (
      "p/profile.yaml",
      "extends: ../base.yaml\nexecutors:\n  here: { workspace: ws/ }\nfiles:\n  seed: { mode: copy-in, from: \"host:./seed/\", to: \"actor:~/seed/\" }\n  out: { mode: mirror-out, from: \"actor:~/out/\", to: \"host:~/out/\" }\n",
    ),
  ]);

  let resolved = resolve(&Source::file(dir.path().join("p/profile.yaml"))).unwrap();
  let profile = load(&Source::text(resolved.yaml.clone(), "")).unwrap();

  assert!(!resolved.yaml.contains("extends"), "{}", resolved.yaml);
  // The profile's directory is this machine's path, with its separators (`\`
  // on Windows); the path the profile wrote follows it as written, with `/`,
  // so a trailing `/` keeps its meaning.
  let base = dir.path().join("p");
  assert_eq!(profile.executors["here"].workspace.as_deref(), Some(format!("{}/ws/", base.display()).as_str()));
  assert_eq!(profile.files.mappings["seed"].from, Side::Host(format!("{}/seed/", base.display())));
  // A container bind mount's source is a host path; a volume's is a name.
  let mounts = &profile.executors["box"].container.as_ref().unwrap().mounts;
  assert!(matches!(&mounts[0], Mount::Bind { source, .. } if *source == format!("{}/cache", base.display())), "{mounts:?}");
  assert!(matches!(&mounts[1], Mount::Volume { source: Some(v), .. } if v == "v"), "{mounts:?}");
  // `~` is the home of the machine that reads the path.
  assert_eq!(profile.files.mappings["out"].to, Side::Host("~/out/".into()));
  assert_eq!(resolved.recipe_yaml, None);
}

#[test]
fn a_resolved_profile_carries_its_recipe_file_but_not_a_built_in_one() {
  let dir = files(&[
    ("r.recipe.yaml", "extends: builtin:gce-ubuntu-22.04\nos: { hostname: h1 }\nnetwork: { metadata: { file: meta.yaml } }\n"),
    ("file.yaml", "actor: { driver: linux-firecracker, recipe: r.recipe.yaml }\n"),
    ("builtin.yaml", "actor: { driver: linux-firecracker, recipe: \"builtin:gce-ubuntu-22.04\" }\n"),
  ]);

  let from_file = resolve(&Source::file(dir.path().join("file.yaml"))).unwrap();
  let built_in = resolve(&Source::file(dir.path().join("builtin.yaml"))).unwrap();

  let recipe = crate::recipe::load(&Source::text(from_file.recipe_yaml.unwrap(), "")).unwrap();
  assert_eq!(recipe.os.hostname.map(|h| h.as_str().to_string()), Some("h1".into()));
  assert_eq!(recipe.network.metadata.and_then(|m| m.file), Some(format!("{}/meta.yaml", dir.path().display())));
  assert_eq!(built_in.recipe_yaml, None);
}

#[test]
fn the_actor_selects_a_runner_by_name_or_labels() {
  let p = load_text("actor: { driver: linux-firecracker, runner: kvm }").unwrap();
  assert_eq!(p.actor.runner, Some(RunnerSelector::Name("kvm".into())));

  let p = load_text("actor: { driver: linux-firecracker, runner: { labels: [kvm] } }").unwrap();
  let kvm = LabelMatch {
    key: "kvm".into(),
    value: None,
  };
  assert_eq!(p.actor.runner, Some(RunnerSelector::Labels(vec![kvm])));

  assert_eq!(load_text("actor: { driver: linux-firecracker }").unwrap().actor.runner, None);
}

// needy never chooses an actor's isolation for the profile: there is no
// default driver, and a profile without one does not load.
#[test]
fn the_actor_driver_is_required() {
  let err = load_text("user: { name: dev }\n").unwrap_err().to_string();
  assert!(err.contains("missing field `actor`"), "{err}");

  let err = load_text("actor: { command: [sh] }\n").unwrap_err().to_string();
  assert!(err.contains("actor: missing field `driver`"), "{err}");

  let err = load_text("actor: { driver: qemu }\n").unwrap_err().to_string();
  assert!(
    err.contains(
      "actor.driver: unknown variant `qemu`, expected one of `linux-firecracker`, `linux-container`, `macos-vz`, `macos-apple-container`"
    ),
    "{err}"
  );
}

#[test]
fn macos_apple_container_is_an_actor_driver() {
  let p = load_text("actor: { driver: macos-apple-container }").unwrap();

  assert_eq!(p.actor.driver, ActorDriver::MacosAppleContainer);
  assert_eq!(p.actor.driver.as_str(), "macos-apple-container");
}

#[test]
fn a_runner_selector_of_another_form_is_rejected() {
  let err = load_text("executors: { mac: { runner: { name: mac } } }").unwrap_err().to_string();
  assert!(err.contains("executors.mac.runner: a runner selector is a runner's name or { labels: [KEY, KEY=VALUE, ...] }"), "{err}");

  let err = load_text("executors: { mac: { runner: { labels: [] } } }").unwrap_err().to_string();
  assert!(err.contains("executors.mac.runner: `labels` selects no runner when empty"), "{err}");

  let err = load_text("executors: { mac: { runner: { labels: [=darwin] } } }").unwrap_err().to_string();
  assert!(err.contains("a label is KEY or KEY=VALUE, not \"=darwin\""), "{err}");

  let err = load_text("executors: { mac: { runner: \"\" } }").unwrap_err().to_string();
  assert!(err.contains("a runner's name is empty"), "{err}");
}

#[test]
fn a_label_match_needs_the_key_and_any_value_it_names() {
  let labels: std::collections::BTreeMap<String, String> = [
    ("os".to_string(), "darwin".to_string()),
    ("kvm".to_string(), String::new()),
  ]
  .into();
  let parse = |text: &str| text.parse::<LabelMatch>().unwrap();

  assert!(parse("kvm").matches(&labels));
  assert!(parse("os").matches(&labels));
  assert!(parse("os=darwin").matches(&labels));
  assert!(!parse("os=linux").matches(&labels));
  assert!(!parse("gpu").matches(&labels));
}

// Remote executors were removed with slice 5 of the runners-and-runs
// design; a profile that still names them gets the replacement, not
// "unknown field".
#[test]
fn a_remote_executor_is_refused_naming_runners_instead() {
  let err = load_text("executors: { mac: { runtime: remote, remote: { url: \"http://127.0.0.1:1\" } } }").unwrap_err().to_string();
  assert!(err.contains("executors.mac.remote: remote executors were removed"), "{err}");
  assert!(err.contains("select it with `runner: NAME`"), "{err}");

  let err = load_text("executors: { mac: { runtime: remote } }").unwrap_err().to_string();
  assert!(err.contains("executors.mac.runtime: remote executors were removed"), "{err}");
}

// The drivers design renamed `actor.runtime`, `executors.NAME.provider` and
// `docker:` without compatibility: the old keys are unknown fields, and
// serde's message lists the new ones.
#[test]
fn the_names_before_drivers_are_unknown_fields() {
  let err = load_text("actor: { runtime: linux-firecracker }").unwrap_err().to_string();
  assert!(err.contains("actor.runtime: unknown field `runtime`, expected one of `driver`"), "{err}");

  let err = load_text("executors: { build: { provider: native } }").unwrap_err().to_string();
  assert!(err.contains("executors.build.provider: unknown field `provider`"), "{err}");

  let err = load_text("executors: { build: { driver: linux-container, docker: { cpus: 1 } } }").unwrap_err().to_string();
  assert!(err.contains("executors.build.docker: unknown field `docker`"), "{err}");

  let err = load_text("executors: { build: { driver: docker } }").unwrap_err().to_string();
  assert!(err.contains("unknown variant `docker`, expected `native` or `linux-container`"), "{err}");
}

// `on_full: new` was removed (owner decision, 2026-11-11): it waited like
// `wait`, since a second instance per key was never built.
#[test]
fn on_full_new_is_refused_naming_wait_and_reject() {
  let err = load_text("executors: { build: { on_full: new } }").unwrap_err().to_string();
  assert!(err.contains("executors.build.on_full"), "{err}");
  assert!(err.contains("unknown variant `new`, expected `wait` or `reject`"), "{err}");
}

#[test]
fn container_run_options_are_typed_fields() {
  let p = load_text(
    r#"
actor: { driver: linux-firecracker }
executors:
  build:
    driver: linux-container
    container:
      cpus: 1.5
      cpu_shares: 512
      cpuset_cpus: "0-3"
      memory: 512MiB
      memory_swap: unlimited
      shm_size: 64MiB
      pids_limit: 512
      ulimits: { nofile: 1024, nproc: { soft: 64, hard: 128 } }
      env: { RUST_LOG: debug }
      workdir: /src
      read_only: true
      init: true
      mounts:
        - { type: bind, source: cache, target: /cache, read_only: true }
        - { type: volume, source: cargo, target: /cargo }
        - { type: tmpfs, target: /tmp, size: 64MiB }
      add_host: { db.example.com: 192.0.2.10 }
      dns: [192.0.2.53]
      sysctls: { net.ipv4.ip_forward: "1" }
      gpus: all
      cap_drop: [NET_RAW]
"#,
  )
  .unwrap();

  let docker = p.executors["build"].container.clone().unwrap();
  assert_eq!(docker.cpus.unwrap().nanos(), 1_500_000_000);
  assert_eq!(docker.cpu_shares, Some(512));
  assert_eq!(docker.cpuset_cpus.as_deref(), Some("0-3"));
  assert_eq!(docker.memory.unwrap().bytes(), 512 << 20);
  assert_eq!(docker.memory_swap, Some(MemorySwap::Unlimited));
  assert_eq!(docker.shm_size.unwrap().bytes(), 64 << 20);
  assert_eq!(docker.pids_limit, Some(512));
  assert_eq!(docker.ulimits["nofile"], Ulimit::Same(1024));
  assert_eq!(
    docker.ulimits["nproc"],
    Ulimit::Pair {
      soft: 64,
      hard: 128
    }
  );
  assert_eq!(docker.env["RUST_LOG"], "debug");
  assert_eq!(docker.workdir.as_deref(), Some("/src"));
  assert_eq!((docker.read_only, docker.init), (Some(true), Some(true)));
  assert_eq!(
    docker.mounts,
    [
      Mount::Bind {
        source: "cache".into(),
        target: "/cache".into(),
        read_only: true
      },
      Mount::Volume {
        source: Some("cargo".into()),
        target: "/cargo".into(),
        read_only: false
      },
      Mount::Tmpfs {
        target: "/tmp".into(),
        size: Some(crate::value::ByteSize::from_mib(64))
      },
    ]
  );
  assert_eq!(docker.add_host["db.example.com"], "192.0.2.10");
  assert_eq!(docker.dns, ["192.0.2.53".parse::<std::net::IpAddr>().unwrap()]);
  assert_eq!(docker.sysctls["net.ipv4.ip_forward"], "1");
  assert_eq!(docker.gpus, Some(Gpus::All));
  assert_eq!(docker.cap_drop, ["NET_RAW"]);
  assert!(!docker.privileged);
}

#[test]
fn gpus_are_all_a_count_or_device_ids() {
  let gpus = |text: &str| {
    load_text(&format!(
      "actor: {{ driver: linux-firecracker }}\nexecutors: {{ build: {{ driver: linux-container, container: {{ gpus: {text} }} }} }}"
    ))
    .map(|p| p.executors["build"].container.clone().unwrap().gpus.unwrap())
    .map_err(|e| e.to_string())
  };
  assert_eq!(gpus("all"), Ok(Gpus::All));
  assert_eq!(gpus("2"), Ok(Gpus::Count(2)));
  assert_eq!(gpus("[\"0\", GPU-example]"), Ok(Gpus::Devices(vec!["0".into(), "GPU-example".into()])));
  let err = gpus("some").unwrap_err();
  assert!(err.contains("executors.build.container.gpus: "), "{err}");
  assert!(err.contains("all, a number of GPUs or a list of device IDs"), "{err}");
}

#[test]
fn malformed_container_values_fail_at_load_with_their_path() {
  let err = |container: &str| {
    load_text(&format!("executors: {{ build: {{ driver: linux-container, container: {container} }} }}")).unwrap_err().to_string()
  };
  let e = err("{ memory: 8g }");
  assert!(e.contains("executors.build.container.memory: invalid size unit"), "{e}");
  let e = err("{ cpus: 0 }");
  assert!(e.contains("executors.build.container.cpus: "), "{e}");
  let e = err("{ memory_swap: lots }");
  assert!(e.contains("executors.build.container.memory_swap: "), "{e}");
  let e = err("{ mounts: [{ type: nfs, target: /x }] }");
  assert!(e.contains("executors.build.container.mounts[0].type: unknown variant `nfs`"), "{e}");
  let e = err("{ mounts: [{ type: tmpfs, target: /x, source: /y }] }");
  assert!(e.contains("unknown field `source`"), "{e}");
  let e = err("{ ulimits: { nofile: { soft: 1 } } }");
  assert!(e.contains("executors.build.container.ulimits.nofile: "), "{e}");
  let e = err("{ dns: [not-an-address] }");
  assert!(e.contains("executors.build.container.dns[0]: "), "{e}");
}

// The daemon sets these; they are not profile fields at all.
#[test]
fn fields_the_daemon_owns_are_not_docker_fields() {
  for field in [
    "hostname: h",
    "user: \"0\"",
    "entrypoint: sh",
    "network: host",
    "name: x",
    "labels: { a: b }",
    "rm: true",
  ] {
    let err =
      load_text(&format!("executors: {{ build: {{ driver: linux-container, container: {{ {field} }} }} }}")).unwrap_err().to_string();
    assert!(err.contains("unknown field"), "{field}: {err}");
  }
}

// `docker.args` (docker run strings) was replaced by typed fields on
// 2026-11-11, without compatibility; `container:` has no `args`.
#[test]
fn container_args_is_an_unknown_field() {
  let err = load_text("executors: { build: { driver: linux-container, container: { args: [--cpus=4] } } }").unwrap_err().to_string();

  assert!(err.contains("unknown field `args`"), "{err}");
}
