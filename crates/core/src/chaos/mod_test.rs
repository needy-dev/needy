use std::time::{Duration, Instant};

use needy_config::profile::{Chaos, FaultDomain, Profile};

use super::*;
use crate::intercept::exec::{Decision, ExecContext};
use crate::intercept::net::{NetContext, NetDecide, NetDecision, NetOutcome};
use crate::intercept::{exec, net as net_domain};
use crate::rules::Rules;
use crate::types::{Effect, Network, Outcome, Placement};

fn chaos(yaml: &str) -> Chaos {
  serde_yaml::from_str(yaml).unwrap()
}

fn fault(yaml: &str) -> Fault {
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

fn tunnel(host: &str) -> Subject<'_> {
  Subject::Net {
    host,
    request: false,
  }
}

#[test]
fn exec_faults_match_commands_word_by_word() {
  let i = Injector::new(&chaos("faults: { a: {exec: 'curl *', exit: 7}, b: {exec: '{git,hg} push **', exit: 1} }"));
  assert!(exec(&i, "/usr/bin/curl https://x/y").is_some());
  assert!(exec(&i, "curl -s https://x/y").is_none());
  assert!(exec(&i, "git push origin main").is_some());
  assert!(exec(&i, "git pull").is_none());
  assert_eq!(i.matching(Subject::Exec(&argv("curl https://x"))).len(), 1);
}

#[test]
fn a_fault_without_a_trigger_fires_on_every_match() {
  let i = Injector::new(&chaos("faults: { down: {exec: 'curl **', exit: 7} }"));
  assert!((0..5).all(|_| exec(&i, "curl x").is_some()));
}

#[test]
fn nth_fires_once() {
  let i = Injector::new(&chaos("faults: { second: {exec: 'curl *', exit: 7, nth: 2} }"));
  assert!(exec(&i, "curl a").is_none());
  assert!(exec(&i, "curl b").is_some());
  assert!(exec(&i, "curl c").is_none());
  assert!(exec(&i, "wget a").is_none());
}

#[test]
fn times_caps_how_often_a_fault_fires() {
  let i = Injector::new(&chaos("faults: { twice: {exec: 'curl **', exit: 7, times: 2} }"));
  let fired: Vec<bool> = (0..4).map(|_| exec(&i, "curl x").is_some()).collect();
  assert_eq!(fired, [true, true, false, false]);
  let state = &i.list()[0];
  assert_eq!((state.matched, state.fired), (4, 2));
}

#[test]
fn a_time_window_counts_from_the_run_s_creation() {
  let yaml = "faults: { late: {exec: 'curl **', exit: 7, after: 10s}, early: {exec: 'wget **', exit: 7, until: 10s} }";
  let young = Injector::since(&chaos(yaml), Instant::now());
  assert!(exec(&young, "curl x").is_none());
  assert!(exec(&young, "wget x").is_some());
  let old = Injector::since(&chaos(yaml), Instant::now() - Duration::from_secs(11));
  assert!(exec(&old, "curl x").is_some());
  assert!(exec(&old, "wget x").is_none());
  // A match outside the window is not counted.
  assert_eq!(young.list().iter().map(|s| (s.name.as_str(), s.matched)).collect::<Vec<_>>(), [("early", 1), ("late", 0)]);
}

#[test]
fn probability_is_reproducible() {
  let yaml = "seed: 42\nfaults: { flaky: {net: {connect: '*'}, refuse: true, probability: 0.3} }";
  let run = || {
    let i = Injector::new(&chaos(yaml));
    (0..200).map(|_| i.check(tunnel("example.com")).is_some()).collect::<Vec<_>>()
  };
  let a = run();
  assert_eq!(a, run());
  let hits = a.iter().filter(|x| **x).count();
  assert!((30..90).contains(&hits), "{hits}");
}

// ROOT CAUSE:
//
// If a fault's roll were mixed with its index among the faults, as with the
// list of faults before 2026-11-12, adding or removing another fault while
// the run ran would shift it and change which operations it fails.
//
// The roll now mixes the fault's name, so a fault's rolls do not depend on
// the other faults.
#[test]
fn another_fault_does_not_change_a_fault_s_rolls() {
  let rolls = |yaml: &str| {
    let i = Injector::new(&chaos(yaml));
    (0..100).map(|_| i.check(tunnel("example.com")).map(|f| f.name)).collect::<Vec<_>>()
  };
  let alone = rolls("seed: 3\nfaults: { m: {net: {connect: '*'}, refuse: true, probability: 0.5} }");
  let with_other = rolls(
    "seed: 3\nfaults: { m: {net: {connect: '*'}, refuse: true, probability: 0.5}, z: {net: {connect: '*'}, refuse: true, nth: 1000} }",
  );
  assert_eq!(alone, with_other);
}

#[test]
fn a_fired_fault_names_itself_its_domain_target_and_effect() {
  let i = Injector::new(&chaos("faults: { slow: {net: {connect: '*.example.com'}, refuse: true, delay: 500ms} }"));
  let fired = i.check(tunnel("api.example.com")).unwrap();
  assert_eq!(
    fired,
    Fired {
      name: "slow".into(),
      domain: FaultDomain::Net,
      target: "api.example.com".into(),
      effect: Effect {
        refuse: true,
        delay: Some(Duration::from_millis(500)),
        ..Effect::default()
      },
    }
  );
}

#[test]
fn a_request_fault_matches_plain_http_requests_only() {
  let i = Injector::new(&chaos("faults: { busy: {net: {request: 'ollama.needy'}, status: 503} }"));
  assert!(i.check(tunnel("ollama.needy")).is_none());
  let fired = i
    .check(Subject::Net {
      host: "OLLAMA.needy",
      request: true,
    })
    .unwrap();
  assert_eq!(fired.effect.status, Some(503));
}

#[test]
fn file_faults_match_the_path_in_the_actor() {
  let i = Injector::new(&chaos(
    "faults: { json: {file: {open: '**/*.json'}, errno: EIO}, src: {file: {open: '/home/agent/workspace/src/**'}, delay: 1s} }",
  ));
  assert_eq!(i.check(Subject::FileOpen("/home/agent/workspace/a.json")).unwrap().name, "json");
  assert_eq!(i.check(Subject::FileOpen("/home/agent/workspace/src/main.rs")).unwrap().name, "src");
  assert!(i.check(Subject::FileOpen("/home/agent/workspace/README")).is_none());
}

#[test]
fn faults_are_added_and_removed_while_a_run_runs() {
  let i = Injector::new(&chaos("faults: { base: {exec: 'curl **', exit: 7, nth: 1000} }"));
  assert!(exec(&i, "wget x").is_none());

  i.add("wget-down", fault("{exec: 'wget **', exit: 4}")).unwrap();
  assert_eq!(i.add("wget-down", fault("{exec: 'wget **', exit: 4}")), Err(AddError::Exists("wget-down".into())));
  assert_eq!(exec(&i, "wget x").unwrap().effect.exit, Some(4));
  let listed: Vec<(String, FaultSource)> = i.list().into_iter().map(|s| (s.name, s.source)).collect();
  assert_eq!(
    listed,
    [
      ("base".to_string(), FaultSource::Profile),
      ("wget-down".to_string(), FaultSource::Added)
    ]
  );

  // A profile's fault can be removed too.
  assert!(i.remove("base").is_some());
  assert!(i.remove("wget-down").is_some());
  assert!(i.remove("wget-down").is_none());
  assert!(exec(&i, "wget x").is_none());
  assert!(i.list().is_empty());
}

#[test]
fn exec_decide_chain_runs_rules_then_chaos() {
  let p = profile(
    r#"
actor: { driver: linux-firecracker }
executors: { build: { driver: linux-container } }
commands:
  - { match: "curl **", when: { has: [-k] }, deny: insecure }
  - { match: "**", to: build }
chaos:
  faults:
    curl-down: { exec: "curl **", exit: 7, stderr: "curl: (7) Failed to connect" }
    slow-sleep: { exec: "sleep **", delay: 1s }
"#,
  );
  let rules = Rules::new(&p);
  let injector = Injector::new(&p.chaos);
  let decide = |command: &str| {
    let ctx = ExecContext {
      argv: argv(command),
      ..Default::default()
    };
    exec::decide(&[&rules, &injector], &ctx, Decision::unchanged(&ctx, Placement::Actor))
  };

  // A command the rules deny is not faulted.
  let d = decide("curl -k https://x");
  assert_eq!(d.outcome, Outcome::Deny("insecure".into()));
  assert!(d.fired.is_empty());

  // An exit ends the command in place of running it, and is recorded.
  let d = decide("curl https://x");
  assert_eq!(d.outcome, Outcome::Fault("curl-down".into()));
  let fired = d.fault().unwrap();
  assert_eq!((fired.target.as_str(), fired.effect.exit), ("curl https://x", Some(7)));
  assert_eq!(fired.effect.stderr.as_deref(), Some("curl: (7) Failed to connect"));

  // A delay keeps the placement; the flow applies it.
  let d = decide("sleep 1");
  assert_eq!(d.outcome, Outcome::Run(Placement::Executor("build".into())));
  assert_eq!(d.fired[0].effect.delay, Some(Duration::from_secs(1)));
  assert!(d.fault().is_none());

  let d = decide("ls");
  assert_eq!(d.outcome, Outcome::Run(Placement::Executor("build".into())));
  assert!(d.fired.is_empty());
}

#[test]
fn chaos_alone_faults_an_explicitly_placed_command() {
  let injector = Injector::new(&chaos("faults: { crash: {exec: 'curl **', signal: SIGSEGV} }"));
  let ctx = ExecContext {
    argv: argv("curl https://x"),
    ..Default::default()
  };
  let d = exec::decide(&[&injector], &ctx, Decision::unchanged(&ctx, Placement::Executor("mac".into())));
  assert_eq!(d.outcome, Outcome::Fault("crash".into()));
  assert_eq!(d.fault().unwrap().effect.signal.as_deref(), Some("SIGSEGV"));
  assert_eq!(d.argv, argv("curl https://x"));
}

/// Denies one host, as an allowlist would.
struct DenyHost(&'static str);

impl NetDecide for DenyHost {
  fn decide(&self, ctx: &NetContext, mut decision: NetDecision) -> NetDecision {
    if ctx.host == self.0 {
      decision.outcome = NetOutcome::Deny;
    }
    decision
  }
}

fn net(host: &str) -> NetContext {
  NetContext {
    network: Network::Actor,
    host: host.into(),
    path: Some("/v1".into()),
  }
}

#[test]
fn net_chain_runs_in_order_and_chaos_skips_a_denied_host() {
  let injector = Injector::new(&chaos("faults: { first: {net: {connect: '*'}, nth: 1, reset: true} }"));
  let chain: [&dyn NetDecide; 2] = [&DenyHost("blocked.example.com"), &injector];
  let decide = |host: &str| net_domain::decide(&chain, &net(host), NetDecision::allowed());

  // A denied host is not counted by chaos.
  let d = decide("blocked.example.com");
  assert_eq!(
    d,
    NetDecision {
      outcome: NetOutcome::Deny,
      ..NetDecision::allowed()
    }
  );

  // So the first allowed request is the fault's first occurrence.
  let d = decide("api.example.com");
  assert_eq!(d.outcome, NetOutcome::Fault("first".into()));
  let fired = d.fault().unwrap();
  assert_eq!((fired.domain, fired.target.as_str(), fired.effect.reset), (FaultDomain::Net, "api.example.com", true));

  let d = decide("api.example.com");
  assert_eq!(d.outcome, NetOutcome::Allow);
}

#[test]
fn a_net_delay_keeps_the_request_allowed() {
  let injector = Injector::new(&chaos("faults: { slow: {net: {connect: 'api.example.com'}, delay: 500ms} }"));

  let d = net_domain::decide(&[&injector], &net("api.example.com"), NetDecision::allowed());

  assert_eq!(d.outcome, NetOutcome::Allow);
  assert_eq!(d.fired[0].effect.delay, Some(Duration::from_millis(500)));
}
