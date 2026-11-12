use std::collections::BTreeMap;
use std::time::Duration;

use needy_config::profile::Profile;

use super::*;
use crate::intercept::ExecContext;
use crate::types::{Outcome, Placement};

fn argv(s: &str) -> Vec<String> {
  s.split_whitespace().map(str::to_string).collect()
}

fn ctx(s: &str) -> ExecContext {
  ExecContext {
    argv: argv(s),
    cwd: "/home/agent/workspace".into(),
    env: BTreeMap::from([
      ("PATH".into(), "/usr/bin".into()),
      ("HOME".into(), "/home/agent".into()),
    ]),
    ..Default::default()
  }
}

fn on(name: &str) -> Outcome {
  Outcome::Run(Placement::Executor(name.into()))
}

fn actor() -> Outcome {
  Outcome::Run(Placement::Actor)
}

fn deny(message: &str) -> Outcome {
  Outcome::Deny(message.into())
}

fn profile(yaml: &str) -> Profile {
  needy_config::profile::load(&needy_config::Source::text(yaml, ".")).unwrap()
}

#[test]
fn composition_example_from_the_doc() {
  let p = profile(
    r#"
actor: { driver: linux-firecracker }
executors: { build: { driver: linux-container }, mac: { runner: mac } }
presets:
  clean-env: { env: { clear: true, pass: [PATH, HOME, LANG, TERM] } }
  mac:       { to: mac }
commands:
  - match: "**"
    env: { set: { CI: "1" } }
  - match: pnpm dlx pi **
    to: actor
    final: true
  - match: "{node,bun} {script} **"
    when: { captures: { script: "**/pi/dist/cli.js" } }
    to: actor
    final: true
  - match: "{uname,lscpu,sw_vers} **"
    to: actor
  - match: curl **
    when: { has: [-k, --insecure] }
    deny: insecure TLS is not allowed
  - match: git **
    args: { prepend: [-c, color.ui=never] }
  - match: "{xcodebuild,codesign} **"
    use: [mac, clean-env]
  - match: "**"
    to: build
    use: [clean-env]
"#,
  );
  let d = decide(&p, &ctx("git status"));
  assert_eq!(d.outcome, on("build"));
  assert_eq!(d.argv, argv("git -c color.ui=never status"));
  assert!(d.clear_env);
  // `clear` limits only the inherited environment; CI=1 from rule 0 stays.
  assert_eq!(d.env.get("CI").map(String::as_str), Some("1"));
  assert!(!d.env.contains_key("SHELL"));
  assert_eq!(d.env.get("PATH").map(String::as_str), Some("/usr/bin"));
  assert_eq!(d.matched, vec![0, 5, 7]);

  let d = decide(&p, &ctx("pnpm dlx pi --help"));
  assert_eq!(d.outcome, actor());
  assert_eq!(d.matched, vec![0, 1]);
  assert_eq!(d.env.get("CI").map(String::as_str), Some("1"));

  let d = decide(&p, &ctx("node /opt/pi/dist/cli.js"));
  assert_eq!(d.outcome, actor());
  let d = decide(&p, &ctx("node server.js"));
  assert_eq!(d.outcome, on("build"));

  assert_eq!(decide(&p, &ctx("uname -a")).outcome, actor());
  assert_eq!(decide(&p, &ctx("curl -k https://x")).outcome, deny("insecure TLS is not allowed"));
  assert_eq!(decide(&p, &ctx("curl https://x")).outcome, on("build"));
  assert_eq!(decide(&p, &ctx("xcodebuild -list")).outcome, on("mac"));
}

#[test]
fn env_args_wrap_exec_and_cwd() {
  let p = profile(
    r#"
actor: { driver: linux-firecracker }
commands:
  - match: "pytest **"
    args: { append: [-p, no:cacheprovider] }
    wrap: timeout 600
    env:
      default: { LANG: C.UTF-8 }
      prepend: { PATH: /opt/tools/bin }
      unset: [HOME]
    cwd: ~/workspace/tests
    timeout: 10m
  - match: "pytest **"
    wrap: [nice]
    env: { append: { PATH: /extra } }
    timeout: 1s
  - match: "curl {url}"
    exec: audited-fetch --log {url}
"#,
  );
  let d = decide(&p, &ctx("pytest -q"));
  assert_eq!(d.argv, argv("timeout 600 nice pytest -q -p no:cacheprovider"));
  assert_eq!(d.env["PATH"], "/opt/tools/bin:/usr/bin:/extra");
  assert_eq!(d.env["LANG"], "C.UTF-8");
  assert!(!d.env.contains_key("HOME"));
  assert_eq!(d.cwd, "/home/agent/workspace/tests");
  assert_eq!(d.timeout, Some(Duration::from_secs(600)));
  assert_eq!(d.outcome, actor());

  let d = decide(&p, &ctx("curl https://e.com"));
  assert_eq!(d.argv, argv("audited-fetch --log https://e.com"));
}

#[test]
fn when_conditions() {
  let p = profile(
    r#"
actor: { driver: linux-firecracker }
default_executor: build
commands:
  - { match: "node **", when: { cwd: "/home/agent/workspace/**" }, to: a }
  - { match: "make **", when: { env: { CI: "1" } }, to: b }
  - { match: "node **", when: { under: "pnpm test **" }, to: c }
  - { match: "vim **", when: { tty: true }, to: d }
  - { match: "git **", when: { has: [--force] }, deny: no force }
"#,
  );
  let mut c = ctx("node x.js");
  c.cwd = "/tmp".into();
  assert_eq!(decide(&p, &c).outcome, on("build"));
  c.ancestors = vec![argv("pnpm test --watch")];
  assert_eq!(decide(&p, &c).outcome, on("c"));
  c.cwd = "/home/agent/workspace/app".into();
  assert_eq!(decide(&p, &c).outcome, on("a"));
  let mut c = ctx("make all");
  assert_eq!(decide(&p, &c).outcome, on("build"));
  c.env.insert("CI".into(), "1".into());
  assert_eq!(decide(&p, &c).outcome, on("b"));
  let mut c = ctx("vim f");
  c.tty = true;
  assert_eq!(decide(&p, &c).outcome, on("d"));
  assert_eq!(decide(&p, &ctx("git push --force=yes")).outcome, deny("no force"));
  assert_eq!(decide(&p, &ctx("git push")).outcome, on("build"));
}

#[test]
fn builtin_launchers() {
  let p = profile("actor: { driver: linux-firecracker }\ncommands: [{ match: \"git **\", to: build }]");
  for cmd in [
    "env -i FOO=1 git status",
    "sudo -u root git status",
    "nice -n 10 git status",
    "timeout -s KILL 30 git status",
    "nohup git status",
    "stdbuf -o L git status",
    "time -p git status",
    "setsid -f git status",
    "corepack git status",
    "pnpm exec git status",
    "npm exec -- git status",
    "uv run --with x git status",
    "env nice -n 5 sudo git log",
  ] {
    let d = decide(&p, &ctx(cmd));
    assert_eq!(d.outcome, on("build"), "{cmd}");
    assert_eq!(d.effective[0], "git", "{cmd}");
    assert_eq!(d.argv, argv(cmd), "argv stays original for {cmd}");
  }
  assert_eq!(decide(&p, &ctx("npm exec git")).outcome, actor());
  assert_eq!(decide(&p, &ctx("python -m git")).outcome, actor());
}

#[test]
fn user_resolvers() {
  let p = profile(
    r#"
actor: { driver: linux-firecracker }
resolvers:
  - { match: "mytool run {script} **", effective: "{script} **" }
  - { match: "make **", sticky: true }
commands:
  - { match: "deploy.sh **", to: build }
  - { match: "make **", to: actor }
"#,
  );
  let d = decide(&p, &ctx("mytool run deploy.sh --prod"));
  assert_eq!(d.effective, argv("deploy.sh --prod"));
  assert_eq!(d.outcome, on("build"));
  assert_eq!(decide(&p, &ctx("make all")).outcome, actor());
}

#[test]
fn shell_scripts() {
  let mut c = ctx("x");
  let p = profile(
    r#"
actor: { driver: linux-firecracker }
unknown: actor
commands:
  - { match: "{git,make} **", to: build }
  - { match: "uname **", to: actor }
  - { match: "rm **", when: { has: [-rf] }, deny: no }
"#,
  );
  c.argv = vec![
    "bash".into(),
    "-lc".into(),
    "git status && make -j4 | tee log".into(),
  ];
  assert_eq!(decide(&p, &c).outcome, on("build"));
  c.argv = vec!["sh".into(), "-c".into(), "git status; ls".into()];
  assert_eq!(decide(&p, &c).outcome, on("build"));
  c.argv = vec!["sh".into(), "-c".into(), "git status; uname -a".into()];
  assert_eq!(decide(&p, &c).outcome, actor());
  c.argv = vec!["sh".into(), "-c".into(), "echo $HOME".into()];
  assert_eq!(decide(&p, &c).outcome, actor());
  c.argv = vec!["sh".into(), "-c".into(), "git pull && rm -rf /".into()];
  assert_eq!(decide(&p, &c).outcome, deny("no"));
  c.argv = vec!["bash".into(), "-c".into(), "CI=1 'git' \"log\" -1".into()];
  assert_eq!(decide(&p, &c).outcome, on("build"));

  let sticky = profile("actor: { driver: linux-firecracker }\nunknown: build\ncommands: [{ match: \"git **\", to: build }]");
  c.argv = vec!["sh".into(), "-c".into(), "git log > out".into()];
  assert_eq!(decide(&sticky, &c).outcome, on("build"));
}

#[test]
fn route_names_from_rules() {
  let p = profile(
    "actor: { driver: linux-firecracker }\ncommands:\n  - { match: \"{git,make} **\", to: build }\n  - { match: uname, to: actor }\n  - { match: \"cargo **\", executor: build }\n",
  );
  let names = route_names(&p).unwrap();
  assert!(names.contains(&"git".to_string()));
  assert!(names.contains(&"cargo".to_string()));
  assert!(names.contains(&"bash".to_string()));
  assert!(names.contains(&"uname".to_string()), "a rule placing on the actor is still asked about");
  assert_eq!(route_names(&profile("actor: { driver: linux-firecracker }\ncommands: [{ match: \"**\", to: build }]")), None);
  assert_eq!(route_names(&profile("actor: { driver: linux-firecracker }\ndefault_executor: build")), None);
  assert_eq!(route_names(&profile("actor: { driver: linux-firecracker }")), Some(vec![]));
}

// ROOT CAUSE:
//
// If a rule had no `to:` (it only denied, or only changed the environment,
// arguments, working directory, timeout or faults), its command never
// reached the daemon from an actor, so the rule silently did nothing there,
// because the route list held only names some rule placed on an executor.
//
// Before the fix, `{ match: "rm -rf **", deny: ... }` left `rm` off the list.
// The fix keeps the name of every rule's command on the list.
#[test]
fn route_names_include_rules_without_a_placement() {
  let deny = profile("actor: { driver: linux-firecracker }\ncommands: [{ match: \"rm -rf **\", deny: \"not here\" }]");
  let names = route_names(&deny).unwrap();
  assert!(names.contains(&"rm".to_string()), "{names:?}");
  assert!(names.contains(&"sh".to_string()), "a shell may run the denied command: {names:?}");

  let env = profile("actor: { driver: linux-firecracker }\ncommands: [{ match: \"npm **\", env: { set: { CI: \"1\" } } }]");
  assert!(route_names(&env).unwrap().contains(&"npm".to_string()));

  let preset = profile(
    "actor: { driver: linux-firecracker }\npresets: { quiet: { timeout: 5s } }\ncommands: [{ match: \"{curl,wget} **\", use: [quiet] }]",
  );
  let names = route_names(&preset).unwrap();
  assert!(names.contains(&"curl".to_string()) && names.contains(&"wget".to_string()), "{names:?}");

  // A pattern that may match any program still asks about every exec.
  assert_eq!(route_names(&profile("actor: { driver: linux-firecracker }\ncommands: [{ match: \"** --force\", deny: no }]")), None);
}

#[test]
fn files_and_faults_accumulate() {
  let p = profile(
    r#"
actor: { driver: linux-firecracker }
commands:
  - { match: "**", files: [a] }
  - match: "xcodebuild **"
    files: [b, a]
    faults: [{ on: exec, errno: EIO }]
"#,
  );
  let d = decide(&p, &ctx("xcodebuild"));
  assert_eq!(d.files, vec!["a".to_string(), "b".to_string()]);
  assert_eq!(d.faults.len(), 1);
}

// ROOT CAUSE:
//
// If a program was named only by a `chaos.faults` entry with `on: exec`, its
// execs never reached the daemon from an actor, so the fault never fired
// there, because the route list held only the names of `commands` rules.
//
// Before the fix, `{ on: exec, match: "curl **", errno: ECONNREFUSED }` with
// no rule naming `curl` left `curl` off the list.
// The fix puts the command names of exec faults' patterns on the list too.
#[test]
fn route_names_include_exec_fault_commands() {
  let p = profile(
    "actor: { driver: linux-firecracker }\nchaos: { faults: [{ on: exec, match: \"curl **\", errno: ECONNREFUSED }, { on: net.connect, match: \"example.com\", errno: ECONNREFUSED }] }\n",
  );
  let names = route_names(&p).unwrap();
  assert!(names.contains(&"curl".to_string()), "{names:?}");
  assert!(names.contains(&"sh".to_string()), "a shell may run the faulted command: {names:?}");
  assert!(!names.contains(&"example.com".to_string()), "{names:?}");

  let alternatives =
    profile("actor: { driver: linux-firecracker }\nchaos: { faults: [{ on: exec, match: \"{wget,git} **\", delay: 1s }] }\n");
  let names = route_names(&alternatives).unwrap();
  assert!(names.contains(&"wget".to_string()) && names.contains(&"git".to_string()), "{names:?}");

  // An exec fault without a pattern, or with one that may match any program,
  // applies to every exec.
  assert_eq!(route_names(&profile("actor: { driver: linux-firecracker }\nchaos: { faults: [{ on: exec, errno: EIO }] }\n")), None);
  assert_eq!(
    route_names(&profile("actor: { driver: linux-firecracker }\nchaos: { faults: [{ on: exec, match: \"** --force\", errno: EIO }] }\n")),
    None
  );

  // Faults on other hook points do not route execs.
  assert_eq!(
    route_names(&profile("actor: { driver: linux-firecracker }\nchaos: { faults: [{ on: file.open, errno: EIO }] }\n")),
    Some(vec![])
  );
}
