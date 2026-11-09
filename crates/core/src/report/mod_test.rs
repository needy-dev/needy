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
    term_fallback: None,
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
  let report = render("7f3a", &actor_session(), Level::Summary, Format::Markdown);
  assert_eq!(report.lines().next(), Some("# Session report: 7f3a (profile: smoke)"));
}

#[test]
fn commands_a_fault_made_fail_count_as_failed() {
  let report = render("7f3a", &actor_session(), Level::Summary, Format::Markdown);
  // Duration | State | Driving program | Routed | Kept | CLI | Failed | Model calls
  assert_eq!(line_after(&report, "| Duration"), "| 5.0s | finished | exit 0 | 1 | 1 | 1 | 2 | 1 |");
  // Routed | Kept | Denied | Fault injected
  assert_eq!(line_after(&report, "| ↪️"), "| 1 | 1 | 1 | 2 |");
}

#[test]
fn kept_on_actor_says_it_counts_only_routed_names() {
  let report = render("7f3a", &actor_session(), Level::Summary, Format::Markdown);
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
  let report = render("7f3a", &every, Level::Summary, Format::Markdown);
  assert!(report.contains("🏠 Kept on actor counts every command the actor ran, except needy's own init and proxy."), "{report}");

  let no_actor: Vec<_> = actor_session().into_iter().filter(|r| !matches!(r.event, Event::ActorConfig(_))).collect();
  let report = render("7f3a", &no_actor, Level::Summary, Format::Markdown);
  assert!(report.contains("🏠 No actor ran in this session."), "{report}");
}

#[test]
fn empty_activity_lines_are_omitted() {
  let report = render("7f3a", &actor_session(), Level::Summary, Format::Markdown);
  assert!(report.contains("- Actor network: api.example.com (1)\n"), "{report}");
  assert!(!report.contains("Executor network"), "{report}");
  // The daemon's own copy-in is not an executor command.
  assert!(report.contains("- Executor commands: git on build (2)\n"), "{report}");
}

#[test]
fn findings_list_denials_and_faults() {
  let report = render("7f3a", &actor_session(), Level::Summary, Format::Markdown);
  assert!(report.contains("- 🚫 Denied command: `git push --force`: no force\n"), "{report}");
  assert!(report.contains("- ℹ️ Injected faults: 2 (exec (2))\n"), "{report}");
}

#[test]
fn detailed_adds_the_timeline() {
  let report = render("7f3a", &actor_session(), Level::Detailed, Format::Markdown);
  assert!(report.contains("| +ms | Event | Details |"), "{report}");
  assert!(report.contains("| 200 | exec.decide |"), "{report}");
  assert!(!render("7f3a", &actor_session(), Level::Summary, Format::Markdown).contains("Event timeline"));
}

#[test]
fn a_trace_with_gaps_is_reported_incomplete_at_the_top() {
  let mut events = actor_session();
  events.insert(3, at(1_220, Event::TraceGap(TraceGap { dropped: 5 })));
  events.insert(6, at(1_350, Event::TraceGap(TraceGap { dropped: 2 })));

  let report = render("7f3a", &events, Level::Summary, Format::Markdown);

  let mut lines = report.lines();
  assert_eq!(lines.next(), Some("# Session report: 7f3a (profile: smoke)"));
  assert_eq!(lines.next(), Some(""));
  assert_eq!(
    lines.next(),
    Some("> ⚠️ The trace is incomplete: 7 events were dropped when the trace queue was full, so the counts below may be low.")
  );
  assert!(!render("7f3a", &actor_session(), Level::Summary, Format::Markdown).contains("incomplete"));
}

#[test]
fn a_failed_session_shows_its_state_and_reason() {
  let events = vec![
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
      3_000,
      Event::SessionFail(SessionFail {
        reason: "executor build: no such image".into(),
      }),
    ),
  ];

  let report = render("7f3a", &events, Level::Summary, Format::Markdown);

  assert_eq!(line_after(&report, "| Duration"), "| 2.0s | failed | - | 0 | 0 | 0 | 0 | 0 |");
  assert!(report.contains("- ❌ Session failed: executor build: no such image\n"), "{report}");
}

#[test]
fn timing_summaries_are_not_activity() {
  let mut events = actor_session();
  let hooks = [(
    "exec.decide".to_string(),
    HookTiming {
      count: 3,
      total_us: 90,
      min_us: 20,
      max_us: 40,
    },
  )];
  events.push(at(
    7_100,
    Event::TraceTiming(TraceTiming {
      hooks: hooks.into(),
    }),
  ));

  assert_eq!(render("7f3a", &events, Level::Summary, Format::Markdown), render("7f3a", &actor_session(), Level::Summary, Format::Markdown));
}

fn create(time_ms: i64) -> Recorded {
  at(
    time_ms,
    Event::SessionCreate(SessionCreate {
      name: "demo".into(),
      group: "demo".into(),
      profile: "smoke".into(),
      executors: vec![],
    }),
  )
}

// ROOT CAUSE:
//
// If a session finished and was then stopped, the report said `stopped`
// while `session ls` said `finished`, because the report took the state of
// the last terminal event and the daemon's state machine keeps the first.
//
// Before the fix, `session.stop` after `actor.exit` overwrote `finished`.
// The fix keeps the first terminal state, as `session ls` does.
#[test]
fn a_session_stopped_after_it_finished_reports_finished() {
  let events = vec![
    create(1_000),
    at(
      4_000,
      Event::ActorExit(ActorExit {
        code: Some(0),
        signal: None,
      }),
    ),
    at(9_000, Event::SessionStop(SessionStop {})),
  ];

  let report = render("7f3a", &events, Level::Summary, Format::Markdown);

  assert_eq!(line_after(&report, "| Duration"), "| 3.0s | finished | exit 0 | 0 | 0 | 0 | 0 | 0 |");
}

// ROOT CAUSE:
//
// If the actor's VM exited without its driving program reporting an exit,
// the daemon's state became `finished` but the report did not count the VM's
// exit as an end, so a later stop made it say `stopped`.
//
// The fix counts `actor.vm_exit` as the end the daemon's state machine
// counts it as.
#[test]
fn a_vm_exit_without_a_driver_exit_reports_finished() {
  let events = vec![
    create(1_000),
    at(
      2_000,
      Event::ActorVmExit(ActorVmExit {
        code: Some(0),
        signal: None,
        error: None,
      }),
    ),
    at(9_000, Event::SessionStop(SessionStop {})),
  ];

  let report = render("7f3a", &events, Level::Summary, Format::Markdown);

  assert_eq!(line_after(&report, "| Duration"), "| 1.0s | finished | - | 0 | 0 | 0 | 0 | 0 |");
}

#[test]
fn a_session_stopped_while_running_reports_stopped() {
  let events = vec![create(1_000), at(3_000, Event::SessionStop(SessionStop {}))];

  let report = render("7f3a", &events, Level::Summary, Format::Markdown);

  assert_eq!(line_after(&report, "| Duration"), "| 2.0s | stopped | - | 0 | 0 | 0 | 0 | 0 |");
}

// ROOT CAUSE:
//
// If a session lasted less than a second, the report said `0m 00s`, because
// it showed whole minutes and seconds only.
//
// Before the fix, the owner's 0.8 s smoke session read as no time at all.
// The fix shows tenths of a second under a minute, and coarser units above.
#[test]
fn a_sub_second_session_shows_its_duration() {
  let events = vec![create(1_000), at(1_800, Event::SessionStop(SessionStop {}))];

  let report = render("7f3a", &events, Level::Summary, Format::Markdown);

  assert_eq!(line_after(&report, "| Duration"), "| 0.8s | stopped | - | 0 | 0 | 0 | 0 | 0 |");
}

#[test]
fn durations_use_units_that_fit_them() {
  assert_eq!(duration(0), "0.0s");
  assert_eq!(duration(840), "0.8s");
  assert_eq!(duration(59_940), "59.9s");
  assert_eq!(duration(60_000), "1m 00s");
  assert_eq!(duration(245_000), "4m 05s");
  assert_eq!(duration(3_725_000), "1h 02m");
}

fn human(events: &[Recorded]) -> String {
  render("7f3a", events, Level::Summary, Format::Human { color: false })
}

#[test]
fn the_human_report_leads_with_the_session_state_and_duration() {
  let report = human(&actor_session());

  let head: Vec<&str> = report.lines().take(4).collect();
  assert_eq!(
    head,
    [
      "Session   7f3a  (profile smoke)",
      "State     finished, driving program exit 0",
      "Duration  5.0s",
      "",
    ]
  );
}

#[test]
fn the_human_report_counts_commands_per_executor_in_aligned_lines() {
  let report = human(&actor_session());

  let commands: Vec<&str> = report.lines().skip_while(|l| *l != "Commands").take_while(|l| !l.is_empty()).collect();
  assert_eq!(
    commands,
    [
      "Commands",
      "  routed to build  1  (1 failed)",
      "  kept on actor    1",
      "  from needy exec  1",
      "  denied           1",
      "  faults           2",
      "  model calls      1",
      "  (kept on actor counts only the names the actor routes: bash, git)",
    ]
  );
  assert!(report.contains("\nTop commands\n  git on build  2  slowest 30 ms\n"), "{report}");
  assert!(report.contains("\nNetwork\n  actor  api.example.com  1\n"), "{report}");
  assert!(report.contains("  denied command: git push --force: no force\n"), "{report}");
  assert!(report.contains("  injected faults: 2 (exec (2))\n"), "{report}");
}

#[test]
fn the_human_report_has_no_markdown() {
  let report = human(&actor_session());

  assert!(!report.contains('|') && !report.contains('#') && !report.contains('`'), "{report}");
}

#[test]
fn the_human_report_omits_empty_sections() {
  let events = vec![create(1_000), at(1_800, Event::SessionStop(SessionStop {}))];

  let report = human(&events);

  assert_eq!(report, "Session   7f3a  (profile smoke)\nState     stopped\nDuration  0.8s\n\nNo findings.\n");
}

#[test]
fn the_human_report_colors_failures_and_faults_only_when_asked() {
  let colored = render("7f3a", &actor_session(), Level::Summary, Format::Human { color: true });

  assert!(colored.contains("\u{1b}[31m(1 failed)\u{1b}[0m"), "{colored}");
  assert!(colored.contains("\u{1b}[33minjected faults: 2 (exec (2))\u{1b}[0m"), "{colored}");
  assert!(!human(&actor_session()).contains('\u{1b}'));
}

#[test]
fn the_human_report_of_a_failed_session_says_why() {
  let events = vec![
    create(1_000),
    at(
      3_000,
      Event::SessionFail(SessionFail {
        reason: "executor build: no such image".into(),
      }),
    ),
  ];

  let report = human(&events);

  assert!(report.contains("\nState     failed\n"), "{report}");
  assert!(report.ends_with("\nFindings\n  session failed: executor build: no such image\n"), "{report}");
}

#[test]
fn the_json_report_summarizes_the_same_counts() {
  let report = render("7f3a", &actor_session(), Level::Summary, Format::Json);

  let json: serde_json::Value = serde_json::from_str(&report).unwrap();
  assert_eq!(json["session"], "7f3a");
  assert_eq!(json["profile"], "smoke");
  assert_eq!(json["state"], "finished");
  assert_eq!(json["duration_ms"], 5_000);
  assert_eq!(json["actor_exit"], serde_json::json!({ "code": 0, "signal": null }));
  assert_eq!(json["route_names"], serde_json::json!(["bash", "git"]));
  let commands = &json["commands"];
  assert_eq!(commands["routed"], serde_json::json!([{ "executor": "build", "commands": 1, "failed": 1 }]));
  assert_eq!(
    (&commands["kept_on_actor"], &commands["cli"], &commands["denied"], &commands["faults"], &commands["failed"], &commands["model_calls"]),
    (
      &serde_json::json!(1),
      &serde_json::json!(1),
      &serde_json::json!(1),
      &serde_json::json!(2),
      &serde_json::json!(2),
      &serde_json::json!(1)
    )
  );
  assert_eq!(json["top_commands"], serde_json::json!([{ "command": "git", "executor": "build", "count": 2, "slowest_ms": 30 }]));
  let kinds: Vec<&str> = json["findings"].as_array().unwrap().iter().map(|f| f["kind"].as_str().unwrap()).collect();
  assert_eq!(kinds, ["probing", "denied_command", "faults"]);
  assert!(json.get("events").is_none(), "the summary level has no timeline");
}

#[test]
fn the_detailed_json_report_adds_the_timeline() {
  let report = render("7f3a", &actor_session(), Level::Detailed, Format::Json);

  let json: serde_json::Value = serde_json::from_str(&report).unwrap();
  let first = &json["events"][0];
  assert_eq!((&first["offset_ms"], &first["kind"]), (&serde_json::json!(0), &serde_json::json!("session.create")));
  assert_eq!(first["payload"]["profile"], "smoke");
}
