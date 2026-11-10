use std::time::Duration;

use needy_config::profile::FaultKind;

use super::*;
use crate::types::{Effect, Fired, Network, Origin, Outcome, Placement};

fn argv(s: &str) -> Vec<String> {
  s.split_whitespace().map(str::to_string).collect()
}

/// One event of every kind.
fn every_kind() -> Vec<Event> {
  vec![
    Event::RunCreate(RunCreate {
      name: "demo".into(),
      group: "g".into(),
      profile: "smoke".into(),
      executors: vec![ExecutorRef {
        name: "build".into(),
        runtime: "docker".into(),
        instance: "needy-build".into(),
      }],
    }),
    Event::RunStop(RunStop {}),
    Event::RunFail(RunFail {
      reason: "executor build: no such image".into(),
    }),
    Event::ActorStart(ActorStart {
      pid: 42,
      vcpus: 4,
      mem_mib: 16384,
    }),
    Event::ActorVmExit(ActorVmExit {
      code: Some(0),
      signal: None,
      error: None,
    }),
    Event::ActorConfig(ActorConfig {
      route_names: Some(argv("bash git")),
      mounts: vec!["/home/agent/workspace".into()],
    }),
    Event::ActorExit(ActorExit {
      code: None,
      signal: Some("SIGKILL".into()),
    }),
    Event::FilesCopyIn(FilesCopyIn {
      name: "src".into(),
      to: FileEnd::Executor {
        name: "build".into(),
        path: "/home/agent/workspace".into(),
      },
      bytes: 10240,
    }),
    Event::FilesMirrorOut(FilesMirrorOut {
      name: "out".into(),
      from: FileEnd::Actor,
      bytes: 512,
      entries: Some(3),
    }),
    Event::ExecDecide(ExecDecide {
      argv: argv("git status"),
      effective: argv("git status"),
      origin: Origin::ActorProxy,
      outcome: Outcome::Run(Placement::Executor("build".into())),
      rules: vec![0, 5],
    }),
    Event::ExecStart(ExecStart {
      process: "p1".into(),
      executor: "build".into(),
      argv: argv("git status"),
      origin: Origin::Cli,
      term_fallback: Some(TermFallback {
        requested: "xterm-ghostty".into(),
        used: "xterm-256color".into(),
      }),
    }),
    Event::ExecExit(ExecExit {
      process: "p1".into(),
      executor: "build".into(),
      argv: argv("git status"),
      origin: Origin::Cli,
      code: Some(7),
      signal: None,
      ms: 12,
    }),
    Event::FaultInjected(Fired {
      kind: FaultKind::Exec,
      target: "curl https://example.com".into(),
      effect: Effect {
        errno: Some("ECONNREFUSED".into()),
        delay: Some(Duration::from_millis(500)),
        temp_mc: None,
      },
    }),
    Event::NetRequest(NetRequest {
      network: Network::Actor,
      host: "api.example.com".into(),
      path: "/v1/chat/completions".into(),
      status: 200,
      ms: 840,
    }),
    Event::NetConnect(NetConnect {
      network: Network::Actor,
      host: "example.com".into(),
      authority: "example.com:443".into(),
    }),
    Event::NetDeny(NetDeny {
      network: Network::Actor,
      host: "example.org".into(),
      path: None,
    }),
    Event::NetError(NetError {
      network: Network::Actor,
      host: "example.net".into(),
      path: Some("/".into()),
      error: "connection refused".into(),
    }),
    Event::TraceGap(TraceGap { dropped: 4 }),
    Event::TraceTiming(TraceTiming {
      hooks: [(
        "exec.decide".to_string(),
        HookTiming {
          count: 2,
          total_us: 50,
          min_us: 20,
          max_us: 30,
        },
      )]
      .into(),
    }),
  ]
}

#[test]
fn every_kind_round_trips_through_the_stored_json() {
  let events = every_kind();
  let kinds: std::collections::BTreeSet<_> = events.iter().map(Event::kind).collect();
  assert_eq!(kinds.len(), EventKind::ALL.len(), "the fixture covers every kind");
  for event in events {
    let kind = event.kind();
    let stored = event.payload();
    let back = Event::decode(kind.as_str(), SCHEMA_VERSION, &stored).unwrap();
    assert_eq!(back, event, "{kind}: {stored}");
  }
}

#[test]
fn payloads_are_plain_json_objects() {
  let event = Event::ExecDecide(ExecDecide {
    argv: argv("curl https://example.com"),
    effective: argv("curl https://example.com"),
    origin: Origin::Cli,
    outcome: Outcome::Fault("ECONNREFUSED".into()),
    rules: vec![],
  });
  assert_eq!(event.kind().as_str(), "exec.decide");
  let json: serde_json::Value = serde_json::from_str(&event.payload()).unwrap();
  assert_eq!(json["origin"], "cli");
  assert_eq!(json["outcome"]["fault"], "ECONNREFUSED");
  assert_eq!(json["argv"][0], "curl");
}

#[test]
fn a_run_stop_carries_no_attributes() {
  assert_eq!(Event::RunStop(RunStop {}).payload(), "{}");
}

#[test]
fn decode_rejects_unknown_kinds_versions_and_fields() {
  assert!(matches!(Event::decode("exec.bogus", SCHEMA_VERSION, "{}"), Err(DecodeError::Kind(_))));
  assert!(matches!(Event::decode("run.stop", 0, "{}"), Err(DecodeError::Version { found: 0, .. })));
  // Rows written before the typed schema stored flat string attributes.
  let old = r#"{"code":"0","signal":""}"#;
  assert!(matches!(Event::decode("actor.exit", SCHEMA_VERSION, old), Err(DecodeError::Payload { .. })));
  assert!(matches!(Event::decode("run.stop", SCHEMA_VERSION, r#"{"":""}"#), Err(DecodeError::Payload { .. })));
}

// `needy-trace` writes these two kinds itself, without knowing core's types;
// the literals are the payloads its tests pin (`dispatch_test.rs`).
#[test]
fn the_trace_crate_s_own_events_decode() {
  let gap = Event::decode("trace.gap", SCHEMA_VERSION, r#"{"dropped":6}"#).unwrap();
  assert_eq!(gap, Event::TraceGap(TraceGap { dropped: 6 }));
  let timing =
    Event::decode("trace.timing", SCHEMA_VERSION, r#"{"hooks":{"exec.decide":{"count":2,"total_us":40,"min_us":10,"max_us":30}}}"#).unwrap();
  let Event::TraceTiming(timing) = timing else {
    panic!("{timing:?}")
  };
  assert_eq!(timing.hooks["exec.decide"].total_us, 40);
}

// An exec start whose terminal type was kept carries no `term_fallback`,
// so traces written before the field existed read the same.
#[test]
fn an_exec_start_without_a_term_fallback_has_no_such_key() {
  let start = Event::ExecStart(ExecStart {
    process: "p1".into(),
    executor: "build".into(),
    argv: argv("ls"),
    origin: Origin::Cli,
    term_fallback: None,
  });

  let payload = start.payload();

  assert!(!payload.contains("term_fallback"), "{payload}");
  assert_eq!(Event::decode("exec.start", SCHEMA_VERSION, &payload).unwrap(), start);
}
