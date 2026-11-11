use needy_config::profile::FaultKind;

use super::*;
use crate::event::*;
use crate::types::{Effect, Fired, Network, Origin, Outcome, Placement, Principal};

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
    by: None,
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

/// An actor run: one routed command that exits 7, one kept on the actor,
/// one denied, one failed by an exec fault, one delayed, one CLI command and
/// one model call.
fn actor_run() -> Vec<Recorded> {
  vec![
    at(
      1_000,
      Event::RunCreate(RunCreate {
        name: "demo".into(),
        group: "demo".into(),
        profile: "smoke".into(),
        executors: vec![],
        by: None,
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
    at(7_000, Event::RunStop(RunStop::default())),
  ]
}

fn line_after<'a>(report: &'a str, header: &str) -> &'a str {
  let mut lines = report.lines().skip_while(|l| !l.starts_with(header));
  lines.nth(2).unwrap_or_else(|| panic!("no row after {header:?} in\n{report}"))
}

#[test]
fn the_title_names_the_profile_not_its_directory() {
  let report = render("7f3a", &actor_run(), Level::Summary, Format::Markdown);
  assert_eq!(report.lines().next(), Some("# Run report: 7f3a (profile: smoke)"));
}

#[test]
fn commands_a_fault_made_fail_count_as_failed() {
  let report = render("7f3a", &actor_run(), Level::Summary, Format::Markdown);
  // Duration | State | Driving program | Routed | Kept | CLI | Failed | Model calls
  assert_eq!(line_after(&report, "| Duration"), "| 5.0s | finished | exit 0 | 1 | 1 | 1 | 2 | 1 |");
  // Routed | Kept | Denied | Fault injected
  assert_eq!(line_after(&report, "| ↪️"), "| 1 | 1 | 1 | 2 |");
}

#[test]
fn kept_on_actor_says_it_counts_only_routed_names() {
  let report = render("7f3a", &actor_run(), Level::Summary, Format::Markdown);
  assert!(
    report.contains(
      "🏠 Kept on actor counts only commands the actor's kernel routed to needy (`bash`, `git`); other commands ran on the actor without a trace event."
    ),
    "{report}"
  );

  let mut every = actor_run();
  every[1].event = Event::ActorConfig(ActorConfig {
    route_names: None,
    mounts: vec![],
  });
  let report = render("7f3a", &every, Level::Summary, Format::Markdown);
  assert!(report.contains("🏠 Kept on actor counts every command the actor ran, except needy's own init and proxy."), "{report}");

  let no_actor: Vec<_> = actor_run().into_iter().filter(|r| !matches!(r.event, Event::ActorConfig(_))).collect();
  let report = render("7f3a", &no_actor, Level::Summary, Format::Markdown);
  assert!(report.contains("🏠 No actor ran in this run."), "{report}");
}

#[test]
fn empty_activity_lines_are_omitted() {
  let report = render("7f3a", &actor_run(), Level::Summary, Format::Markdown);
  assert!(report.contains("- Actor network: api.example.com (1)\n"), "{report}");
  assert!(!report.contains("Executor network"), "{report}");
  // The daemon's own copy-in is not an executor command.
  assert!(report.contains("- Executor commands: git on build (2)\n"), "{report}");
}

#[test]
fn findings_list_denials_and_faults() {
  let report = render("7f3a", &actor_run(), Level::Summary, Format::Markdown);
  assert!(report.contains("- 🚫 Denied command: `git push --force`: no force\n"), "{report}");
  assert!(report.contains("- ℹ️ Injected faults: 2 (exec (2))\n"), "{report}");
}

#[test]
fn detailed_adds_the_timeline() {
  let report = render("7f3a", &actor_run(), Level::Detailed, Format::Markdown);
  assert!(report.contains("| +ms | Event | Details |"), "{report}");
  assert!(report.contains("| 200 | exec.decide |"), "{report}");
  assert!(!render("7f3a", &actor_run(), Level::Summary, Format::Markdown).contains("Event timeline"));
}

#[test]
fn a_trace_with_gaps_is_reported_incomplete_at_the_top() {
  let mut events = actor_run();
  events.insert(3, at(1_220, Event::TraceGap(TraceGap { dropped: 5 })));
  events.insert(6, at(1_350, Event::TraceGap(TraceGap { dropped: 2 })));

  let report = render("7f3a", &events, Level::Summary, Format::Markdown);

  let mut lines = report.lines();
  assert_eq!(lines.next(), Some("# Run report: 7f3a (profile: smoke)"));
  assert_eq!(lines.next(), Some(""));
  assert_eq!(
    lines.next(),
    Some("> ⚠️ The trace is incomplete: 7 events were dropped when the trace queue was full, so the counts below may be low.")
  );
  assert!(!render("7f3a", &actor_run(), Level::Summary, Format::Markdown).contains("incomplete"));
}

#[test]
fn a_failed_run_shows_its_state_and_reason() {
  let events = vec![
    at(
      1_000,
      Event::RunCreate(RunCreate {
        name: "demo".into(),
        group: "demo".into(),
        profile: "smoke".into(),
        executors: vec![],
        by: None,
      }),
    ),
    at(
      3_000,
      Event::RunFail(RunFail {
        reason: "executor build: no such image".into(),
      }),
    ),
  ];

  let report = render("7f3a", &events, Level::Summary, Format::Markdown);

  assert_eq!(line_after(&report, "| Duration"), "| 2.0s | failed | - | 0 | 0 | 0 | 0 | 0 |");
  assert!(report.contains("- ❌ Run failed: executor build: no such image\n"), "{report}");
}

#[test]
fn timing_summaries_are_not_activity() {
  let mut events = actor_run();
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

  assert_eq!(render("7f3a", &events, Level::Summary, Format::Markdown), render("7f3a", &actor_run(), Level::Summary, Format::Markdown));
}

fn create(time_ms: i64) -> Recorded {
  at(
    time_ms,
    Event::RunCreate(RunCreate {
      name: "demo".into(),
      group: "demo".into(),
      profile: "smoke".into(),
      executors: vec![],
      by: None,
    }),
  )
}

// ROOT CAUSE:
//
// If a run finished and was then stopped, the report said `stopped`
// while `run ls` said `finished`, because the report took the state of
// the last terminal event and the daemon's state machine keeps the first.
//
// Before the fix, `run.stop` after `actor.exit` overwrote `finished`.
// The fix keeps the first terminal state, as `run ls` does.
#[test]
fn a_run_stopped_after_it_finished_reports_finished() {
  let events = vec![
    create(1_000),
    at(
      4_000,
      Event::ActorExit(ActorExit {
        code: Some(0),
        signal: None,
      }),
    ),
    at(9_000, Event::RunStop(RunStop::default())),
  ];

  let report = render("7f3a", &events, Level::Summary, Format::Markdown);

  assert_eq!(line_after(&report, "| Duration"), "| 3.0s | finished | exit 0 | 0 | 0 | 0 | 0 | 0 |");
}

// ROOT CAUSE:
//
// If the actor's VM exited without its driving program reporting an exit,
// such as a Firecracker that refused its CPU template or found no KVM, the
// report said `finished` with no findings, because it counted the VM's exit
// as a finish.
//
// Before the fix, an actor that never started read as a clean run.
// The fix counts a VM exit before any `actor.exit` as a failure, which the
// daemon records as `run.fail` with the reason; a later stop keeps it.
#[test]
fn a_vm_exit_without_a_driver_exit_reports_failed_with_the_reason() {
  let events = vec![
    create(1_000),
    at(
      2_000,
      Event::ActorVmExit(ActorVmExit {
        code: Some(1),
        signal: None,
        error: None,
      }),
    ),
    at(
      2_000,
      Event::RunFail(RunFail {
        reason: "the actor's VM exited before its driving program did: firecracker exited with code 1".into(),
      }),
    ),
    at(9_000, Event::RunStop(RunStop::default())),
  ];

  let report = render("7f3a", &events, Level::Summary, Format::Markdown);

  assert_eq!(line_after(&report, "| Duration"), "| 1.0s | failed | - | 0 | 0 | 0 | 0 | 0 |");
  assert!(
    report.contains("- ❌ Run failed: the actor's VM exited before its driving program did: firecracker exited with code 1\n"),
    "{report}"
  );
}

#[test]
fn a_vm_exit_after_the_driver_exit_reports_finished() {
  let events = vec![
    create(1_000),
    at(
      2_000,
      Event::ActorExit(ActorExit {
        code: Some(0),
        signal: None,
      }),
    ),
    at(
      2_100,
      Event::ActorVmExit(ActorVmExit {
        code: Some(0),
        signal: None,
        error: None,
      }),
    ),
  ];

  let report = render("7f3a", &events, Level::Summary, Format::Markdown);

  assert_eq!(line_after(&report, "| Duration"), "| 1.0s | finished | exit 0 | 0 | 0 | 0 | 0 | 0 |");
}

#[test]
fn a_run_stopped_while_running_reports_stopped() {
  let events = vec![create(1_000), at(3_000, Event::RunStop(RunStop::default()))];

  let report = render("7f3a", &events, Level::Summary, Format::Markdown);

  assert_eq!(line_after(&report, "| Duration"), "| 2.0s | stopped | - | 0 | 0 | 0 | 0 | 0 |");
}

// ROOT CAUSE:
//
// If a run lasted less than a second, the report said `0m 00s`, because
// it showed whole minutes and seconds only.
//
// Before the fix, the owner's 0.8 s smoke run read as no time at all.
// The fix shows tenths of a second under a minute, and coarser units above.
#[test]
fn a_sub_second_run_shows_its_duration() {
  let events = vec![create(1_000), at(1_800, Event::RunStop(RunStop::default()))];

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
fn the_human_report_leads_with_the_run_state_and_duration() {
  let report = human(&actor_run());

  let head: Vec<&str> = report.lines().take(4).collect();
  assert_eq!(
    head,
    [
      "Run       7f3a  (profile smoke)",
      "State     finished, driving program exit 0",
      "Duration  5.0s",
      "",
    ]
  );
}

#[test]
fn the_human_report_counts_commands_per_executor_in_aligned_lines() {
  let report = human(&actor_run());

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

// ROOT CAUSE:
//
// If the actor routed no command names, the note under "Commands" joined an
// empty list: `(kept on actor counts only the names the actor routes: )`.
//
// Before the fix, the note printed the list whatever its length.
// The fix says that the actor routes no names, so nothing kept on it is
// counted, in both the human and the Markdown report.
#[test]
fn an_actor_that_routes_no_names_says_so_instead_of_an_empty_list() {
  let mut events = actor_run();
  events[1].event = Event::ActorConfig(ActorConfig {
    route_names: Some(vec![]),
    mounts: vec![],
  });

  let report = human(&events);
  assert!(!report.contains("routes: )"), "{report}");
  assert!(report.contains("  (the actor routes no command names, so kept on actor counts none of its commands)\n"), "{report}");

  let report = render("7f3a", &events, Level::Summary, Format::Markdown);
  assert!(!report.contains("needy ()"), "{report}");
  assert!(
    report.contains("🏠 The actor routes no command names to needy, so commands it ran are not counted as kept on actor."),
    "{report}"
  );
}

#[test]
fn the_human_report_has_no_markdown() {
  let report = human(&actor_run());

  assert!(!report.contains('|') && !report.contains('#') && !report.contains('`'), "{report}");
}

#[test]
fn the_human_report_omits_empty_sections() {
  let events = vec![create(1_000), at(1_800, Event::RunStop(RunStop::default()))];

  let report = human(&events);

  assert_eq!(report, "Run       7f3a  (profile smoke)\nState     stopped\nDuration  0.8s\n\nNo findings.\n");
}

#[test]
fn the_human_report_colors_failures_and_faults_only_when_asked() {
  let colored = render("7f3a", &actor_run(), Level::Summary, Format::Human { color: true });

  assert!(colored.contains("\u{1b}[31m(1 failed)\u{1b}[0m"), "{colored}");
  assert!(colored.contains("\u{1b}[33minjected faults: 2 (exec (2))\u{1b}[0m"), "{colored}");
  assert!(!human(&actor_run()).contains('\u{1b}'));
}

#[test]
fn the_human_report_of_a_failed_run_says_why() {
  let events = vec![
    create(1_000),
    at(
      3_000,
      Event::RunFail(RunFail {
        reason: "executor build: no such image".into(),
      }),
    ),
  ];

  let report = human(&events);

  assert!(report.contains("\nState     failed\n"), "{report}");
  assert!(report.ends_with("\nFindings\n  run failed: executor build: no such image\n"), "{report}");
}

// ROOT CAUSE:
//
// If a mirror-out failed, such as one whose `host:` path named a directory
// of a remote client's machine, the control daemon only logged a warning,
// so `needy report` said `No findings` and the results were lost unseen.
//
// Before the fix, the trace had no record of the failure.
// The fix records `files.fail`, which the report lists as a finding.
#[test]
fn a_failed_file_transfer_is_a_finding() {
  let events = vec![
    create(1_000),
    at(
      2_000,
      Event::FilesFail(FilesFail {
        name: "results".into(),
        transfer: Transfer::MirrorOut,
        reason: "host path /Users/user/out is not on the control host".into(),
      }),
    ),
    at(3_000, Event::RunStop(RunStop { by: None })),
  ];

  let human = human(&events);
  let markdown = render("7f3a", &events, Level::Summary, Format::Markdown);
  let json: serde_json::Value = serde_json::from_str(&render("7f3a", &events, Level::Summary, Format::Json)).unwrap();

  assert!(human.ends_with("\nFindings\n  mirror-out results failed: host path /Users/user/out is not on the control host\n"), "{human}");
  assert!(markdown.contains("- ❌ Mirror-out `results` failed: host path /Users/user/out is not on the control host\n"), "{markdown}");
  assert_eq!(json["findings"][0]["kind"], "files_failed");
}

#[test]
fn the_json_report_summarizes_the_same_counts() {
  let report = render("7f3a", &actor_run(), Level::Summary, Format::Json);

  let json: serde_json::Value = serde_json::from_str(&report).unwrap();
  assert_eq!(json["run"], "7f3a");
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
  let report = render("7f3a", &actor_run(), Level::Detailed, Format::Json);

  let json: serde_json::Value = serde_json::from_str(&report).unwrap();
  let first = &json["events"][0];
  assert_eq!((&first["offset_ms"], &first["kind"]), (&serde_json::json!(0), &serde_json::json!("run.create")));
  assert_eq!(first["payload"]["profile"], "smoke");
}

fn by(principal: Principal) -> Option<Principal> {
  Some(principal)
}

/// A run a client created and the operator stopped.
fn attributed_run() -> Vec<Recorded> {
  vec![
    at(
      1_000,
      Event::RunCreate(RunCreate {
        name: "demo".into(),
        group: "demo".into(),
        profile: "smoke".into(),
        executors: vec![],
        by: by(Principal::Client("laptop".into())),
      }),
    ),
    at(
      2_000,
      Event::RunStop(RunStop {
        by: by(Principal::Operator),
      }),
    ),
  ]
}

#[test]
fn the_human_report_says_who_started_and_stopped_the_run() {
  let report = human(&attributed_run());

  let head: Vec<&str> = report.lines().take(5).collect();
  assert_eq!(
    head,
    [
      "Run         7f3a  (profile smoke)",
      "State       stopped",
      "Duration    1.0s",
      "Started by  client:laptop",
      "Stopped by  operator",
    ]
  );
}

#[test]
fn the_json_report_names_who_started_and_stopped_the_run() {
  let report = render("7f3a", &attributed_run(), Level::Summary, Format::Json);
  let json: serde_json::Value = serde_json::from_str(&report).unwrap();

  assert_eq!(json["started_by"], "client:laptop");
  assert_eq!(json["stopped_by"], "operator");
}

#[test]
fn a_report_of_a_trace_without_principals_leaves_them_out() {
  let report = human(&actor_run());
  assert!(!report.contains("Started by"), "{report}");

  let json: serde_json::Value = serde_json::from_str(&render("7f3a", &actor_run(), Level::Summary, Format::Json)).unwrap();
  assert!(json["started_by"].is_null());
}

fn container_run() -> Vec<Recorded> {
  let mut events = actor_run();
  events.insert(
    1,
    at(
      1_050,
      Event::ActorStart(ActorStart {
        pid: 7,
        vcpus: 2,
        mem_mib: 4096,
        runtime: Some("container (runsc)".into()),
      }),
    ),
  );
  events
}

#[test]
fn the_report_names_what_ran_the_actor() {
  let report = human(&container_run());

  assert!(report.contains("\nActor     container (runsc)\n"), "{report}");
  let json: serde_json::Value = serde_json::from_str(&render("7f3a", &container_run(), Level::Summary, Format::Json)).unwrap();
  assert_eq!(json["actor_runtime"], "container (runsc)");
}

#[test]
fn a_container_actors_report_says_what_its_routing_leaves_out() {
  let report = human(&container_run());
  assert!(report.contains("programs created at run time and run directly stayed in the actor"), "{report}");

  let report = render("7f3a", &container_run(), Level::Summary, Format::Markdown);
  assert!(report.contains("The actor ran in `container (runsc)`."), "{report}");
  assert!(report.contains("A container actor routes only the programs its image had when it was built"), "{report}");

  assert!(!human(&actor_run()).contains("container actor"), "a run without the runtime says nothing of it");
}
