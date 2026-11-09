use needy_config::profile::FaultKind;

use super::*;
use crate::event::*;
use crate::types::{Effect, Fired, Network, Origin, Outcome, Placement};

fn argv(s: &str) -> Vec<String> {
  s.split_whitespace().map(str::to_string).collect()
}

fn at(time_ms: i64, event: Event) -> Recorded {
  Recorded { time_ms, event }
}

fn decide(origin: Origin, command: &str, outcome: Outcome) -> Event {
  Event::ExecDecide(ExecDecide {
    argv: argv(command),
    effective: argv(command),
    origin,
    outcome,
    rules: vec![],
  })
}

fn exit(origin: Origin, command: &str, code: i32) -> Event {
  Event::ExecExit(ExecExit {
    process: "p".into(),
    executor: "build".into(),
    argv: argv(command),
    origin,
    code: Some(code),
    signal: None,
    ms: 30,
  })
}

fn start(origin: Origin, command: &str) -> Event {
  Event::ExecStart(ExecStart {
    process: "p".into(),
    executor: "build".into(),
    argv: argv(command),
    origin,
    cast: None,
  })
}

fn exec_fault(command: &str, errno: Option<&str>) -> Event {
  Event::FaultInjected(Fired {
    kind: FaultKind::Exec,
    target: command.into(),
    effect: Effect {
      errno: errno.map(Into::into),
      ..Effect::default()
    },
  })
}

/// An actor session: one routed command that exits 7, one kept on the actor,
/// one denied, one failed by an exec fault, one delayed, one CLI command and
/// one model call.
fn actor_session() -> Vec<Recorded> {
  vec![
    at(
      1_000,
      Event::SessionCreate(SessionCreate {
        name: "demo".into(),
        group: "demo".into(),
        profile: "smoke".into(),
        executors: vec![],
      }),
    ),
    at(
      1_100,
      Event::ActorConfig(ActorConfig {
        route_names: Some(argv("bash git")),
        mounts: vec![],
      }),
    ),
    at(1_200, decide(Origin::ActorProxy, "git status", Outcome::Run(Placement::Executor("build".into())))),
    at(1_210, start(Origin::ActorProxy, "git status")),
    at(1_240, exit(Origin::ActorProxy, "git status", 7)),
    at(1_300, decide(Origin::ActorProxy, "bash -c uname", Outcome::Run(Placement::Actor))),
    at(1_400, decide(Origin::ActorProxy, "git push --force", Outcome::Deny("no force".into()))),
    at(1_500, exec_fault("curl https://example.com", Some("ECONNREFUSED"))),
    at(1_500, decide(Origin::ActorProxy, "curl https://example.com", Outcome::Fault("ECONNREFUSED".into()))),
    at(1_600, exec_fault("git fetch", None)),
    at(1_700, start(Origin::Cli, "git log")),
    at(1_720, exit(Origin::Cli, "git log", 0)),
    at(1_750, start(Origin::Files, "tar -x -f -")),
    at(
      1_800,
      Event::NetRequest(NetRequest {
        network: Network::Actor,
        host: "api.example.com".into(),
        path: "/v1/chat/completions".into(),
        status: 200,
        ms: 900,
      }),
    ),
    at(
      6_000,
      Event::ActorExit(ActorExit {
        code: Some(0),
        signal: None,
      }),
    ),
    at(7_000, Event::SessionStop(SessionStop {})),
  ]
}

fn line_after<'a>(report: &'a str, header: &str) -> &'a str {
  let mut lines = report.lines().skip_while(|l| !l.starts_with(header));
  lines.nth(2).unwrap_or_else(|| panic!("no row after {header:?} in\n{report}"))
}

#[test]
fn the_title_names_the_profile_not_its_directory() {
  let report = render("7f3a", &actor_session(), Level::Summary);
  assert_eq!(report.lines().next(), Some("# Session report: 7f3a (profile: smoke)"));
}

#[test]
fn commands_a_fault_made_fail_count_as_failed() {
  let report = render("7f3a", &actor_session(), Level::Summary);
  // Duration | State | Driving program | Routed | Kept | CLI | Failed | Model calls
  assert_eq!(line_after(&report, "| Duration"), "| 0m 05s | stopped | exit 0 | 1 | 1 | 1 | 2 | 1 |");
  // Routed | Kept | Denied | Fault injected
  assert_eq!(line_after(&report, "| ↪️"), "| 1 | 1 | 1 | 2 |");
}

#[test]
fn kept_on_actor_says_it_counts_only_routed_names() {
  let report = render("7f3a", &actor_session(), Level::Summary);
  assert!(
    report.contains(
      "🏠 Kept on actor counts only commands the actor's kernel routed to needy (`bash`, `git`); other commands ran on the actor without a trace event."
    ),
    "{report}"
  );

  let mut every = actor_session();
  every[1].event = Event::ActorConfig(ActorConfig {
    route_names: None,
    mounts: vec![],
  });
  let report = render("7f3a", &every, Level::Summary);
  assert!(report.contains("🏠 Kept on actor counts every command the actor ran, except needy's own init and proxy."), "{report}");

  let no_actor: Vec<_> = actor_session().into_iter().filter(|r| !matches!(r.event, Event::ActorConfig(_))).collect();
  let report = render("7f3a", &no_actor, Level::Summary);
  assert!(report.contains("🏠 No actor ran in this session."), "{report}");
}

#[test]
fn empty_activity_lines_are_omitted() {
  let report = render("7f3a", &actor_session(), Level::Summary);
  assert!(report.contains("- Actor network: api.example.com (1)\n"), "{report}");
  assert!(!report.contains("Executor network"), "{report}");
  // The daemon's own copy-in is not an executor command.
  assert!(report.contains("- Executor commands: git on build (2)\n"), "{report}");
}

#[test]
fn findings_list_denials_and_faults() {
  let report = render("7f3a", &actor_session(), Level::Summary);
  assert!(report.contains("- 🚫 Denied command: `git push --force`: no force\n"), "{report}");
  assert!(report.contains("- ℹ️ Injected faults: 2 (exec (2))\n"), "{report}");
}

#[test]
fn detailed_adds_the_timeline() {
  let report = render("7f3a", &actor_session(), Level::Detailed);
  assert!(report.contains("| +ms | Event | Details |"), "{report}");
  assert!(report.contains("| 200 | exec.decide |"), "{report}");
  assert!(!render("7f3a", &actor_session(), Level::Summary).contains("Event timeline"));
}
