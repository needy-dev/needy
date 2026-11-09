use std::time::Duration;

use needy_config::profile::{Chaos, FaultKind, Profile};

use super::*;
use crate::intercept::{self, ExecContext};
use crate::rules::Rules;
use crate::types::{Decision, Effect, Outcome, Placement};

fn chaos(yaml: &str) -> Chaos {
  serde_yaml::from_str(yaml).unwrap()
}

fn argv(s: &str) -> Vec<String> {
  s.split_whitespace().map(str::to_string).collect()
}

fn profile(yaml: &str) -> Profile {
  needy_config::profile::load(&needy_config::Source::text(yaml, ".")).unwrap()
}

fn exec(i: &Injector, command: &str) -> Option<Fired> {
  i.check(Subject::Exec(&argv(command)))
}

#[test]
fn exec_faults_match_commands_word_by_word() {
  let i = Injector::new(&chaos("faults: [{on: exec, match: 'curl *'}, {on: exec, match: '{git,hg} push **'}]"));
  assert!(exec(&i, "/usr/bin/curl https://x/y").is_some());
  assert!(exec(&i, "curl -s https://x/y").is_none());
  assert!(exec(&i, "git push origin main").is_some());
  assert!(exec(&i, "git pull").is_none());
  assert_eq!(i.matching(Subject::Exec(&argv("curl https://x"))).len(), 1);
}

#[test]
fn nth_fires_once() {
  let i = Injector::new(&chaos("faults: [{on: exec, match: 'curl *', nth: 2}]"));
  assert!(exec(&i, "curl a").is_none());
  assert!(exec(&i, "curl b").is_some());
  assert!(exec(&i, "curl c").is_none());
  assert!(exec(&i, "wget a").is_none());
}

#[test]
fn probability_is_reproducible() {
  let yaml = "seed: 42\nfaults: [{on: net.connect, probability: 0.3}]";
  let run = || {
    let i = Injector::new(&chaos(yaml));
    (0..200).map(|_| i.check(Subject::NetConnect("example.com")).is_some()).collect::<Vec<_>>()
  };
  let a = run();
  assert_eq!(a, run());
  let hits = a.iter().filter(|x| **x).count();
  assert!((30..90).contains(&hits), "{hits}");
}

#[test]
fn a_fired_fault_names_its_kind_target_and_effect() {
  let i = Injector::new(&chaos("faults: [{on: net.connect, match: '*.example.com', errno: ECONNREFUSED, delay: 500ms}]"));
  let fired = i.check(Subject::NetConnect("api.example.com")).unwrap();
  assert_eq!(
    fired,
    Fired {
      kind: FaultKind::NetConnect,
      target: "api.example.com".into(),
      effect: Effect {
        errno: Some("ECONNREFUSED".into()),
        delay: Some(Duration::from_millis(500)),
        temp_mc: None,
      },
    }
  );
}

#[test]
fn exec_decide_chain_runs_rules_then_chaos() {
  let p = profile(
    r#"
executors: { build: { runtime: docker } }
commands:
  - { match: "curl **", when: { has: [-k] }, deny: insecure }
  - { match: "**", to: build }
chaos:
  faults:
    - { on: exec, match: "curl **", errno: ECONNREFUSED }
    - { on: exec, match: "sleep **", delay: 1s }
"#,
  );
  let rules = Rules::new(&p);
  let injector = Injector::new(&p.chaos);
  let decide = |command: &str| {
    let ctx = ExecContext {
      argv: argv(command),
      ..Default::default()
    };
    intercept::decide(&[&rules, &injector], &ctx, Decision::unchanged(&ctx, Placement::Actor))
  };

  // A command the rules deny is not faulted.
  let d = decide("curl -k https://x");
  assert_eq!(d.outcome, Outcome::Deny("insecure".into()));
  assert!(d.fired.is_empty());

  // An errno fails the command and is recorded as fired.
  let d = decide("curl https://x");
  assert_eq!(d.outcome, Outcome::Fault("ECONNREFUSED".into()));
  assert_eq!(d.fired.len(), 1);
  assert_eq!(d.fired[0].target, "curl https://x");

  // A delay keeps the placement; the flow applies it.
  let d = decide("sleep 1");
  assert_eq!(d.outcome, Outcome::Run(Placement::Executor("build".into())));
  assert_eq!(d.fired[0].effect.delay, Some(Duration::from_secs(1)));

  let d = decide("ls");
  assert_eq!(d.outcome, Outcome::Run(Placement::Executor("build".into())));
  assert!(d.fired.is_empty());
}

#[test]
fn chaos_alone_faults_an_explicitly_placed_command() {
  let injector = Injector::new(&chaos("faults: [{on: exec, match: 'curl **', errno: EIO}]"));
  let ctx = ExecContext {
    argv: argv("curl https://x"),
    ..Default::default()
  };
  let d = intercept::decide(&[&injector], &ctx, Decision::unchanged(&ctx, Placement::Executor("mac".into())));
  assert_eq!(d.outcome, Outcome::Fault("EIO".into()));
  assert_eq!(d.argv, argv("curl https://x"));
}
