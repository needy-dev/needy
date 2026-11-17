use std::time::Duration;

use needy_config::profile::FaultDomain;

use super::*;
use crate::event::{ChaosAdd, ChaosRemove};
use crate::types::{Capture, Effect, Fired, Network, Origin, Outcome, Placement, Principal};

fn argv(s: &str) -> Vec<String> {
  s.split_whitespace().map(str::to_string).collect()
}

fn store() -> Store {
  Store {
    path: "/home/user/.local/share/needy/files/pi".into(),
    account: "user".into(),
  }
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
        driver: "linux-container".into(),
        instance: "needy-build".into(),
      }],
      by: Some(Principal::Client("laptop".into())),
      recipe_time: Some("20261112-010203".into()),
    }),
    Event::RunStop(RunStop {
      ended: false,
      by: Some(Principal::Operator),
      actor: None,
    }),
    Event::RunStop(RunStop {
      ended: false,
      by: None,
      actor: Some(ActorStop::Graceful),
    }),
    Event::RunStop(RunStop {
      ended: false,
      by: None,
      actor: Some(ActorStop::Forced {
        reason: "asked to stop at once".into(),
      }),
    }),
    Event::RunFail(RunFail {
      reason: "executor build: no such image".into(),
    }),
    Event::ActorStart(ActorStart {
      pid: 42,
      vcpus: 4,
      mem_mib: 16384,
      driver: Some("linux-container (runsc)".into()),
      capture: Some(Capture::Shims),
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
    Event::ActorReady(ActorReady {}),
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
    Event::FilesFail(FilesFail {
      name: "out".into(),
      transfer: Transfer::MirrorOut,
      reason: "permission denied".into(),
    }),
    Event::FilesSync(FilesSync {
      name: "pi".into(),
      store: store(),
      entries: 4,
    }),
    Event::FilesMerge(FilesMerge {
      name: "pi".into(),
      store: store(),
      taken: 2,
      resolved: 1,
      conflicts: vec!["installation_id".into()],
      kept: Some("/home/user/.local/share/needy/conflicts/abc/pi".into()),
    }),
    Event::FilesMergePending(FilesMergePending {
      name: "pi".into(),
      store: store(),
      reason: "the store is unreachable".into(),
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
      by: Some(Principal::Actor("0a1b2c3d".into())),
    }),
    Event::ExecKill(ExecKill {
      process: "p1".into(),
      executor: "build".into(),
      signal: "TERM".into(),
      by: Some(Principal::Client("laptop".into())),
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
      name: "curl-down".into(),
      domain: FaultDomain::Exec,
      target: "curl https://example.com".into(),
      effect: Effect {
        exit: Some(7),
        stderr: Some("curl: (7) Failed to connect".into()),
        delay: Some(Duration::from_millis(500)),
        ..Effect::default()
      },
    }),
    Event::ChaosAdd(ChaosAdd {
      name: "slow-npm".into(),
      fault: serde_yaml::from_str("{ net: { connect: registry.npmjs.org }, delay: 2s, times: 3 }").unwrap(),
      by: Some(Principal::Client("laptop".into())),
    }),
    Event::ChaosRemove(ChaosRemove {
      name: "slow-npm".into(),
      by: Some(Principal::Operator),
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
    outcome: Outcome::Fault("curl-down".into()),
    rules: vec![],
  });
  assert_eq!(event.kind().as_str(), "exec.decide");
  let json: serde_json::Value = serde_json::from_str(&event.payload()).unwrap();
  assert_eq!(json["origin"], "cli");
  assert_eq!(json["outcome"]["fault"], "curl-down");
  assert_eq!(json["argv"][0], "curl");
}

#[test]
fn a_cleanup_stop_records_that_it_did_not_establish_the_terminal_state() {
  assert_eq!(Event::RunStop(RunStop::default()).payload(), r#"{"ended":false}"#);
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
    by: None,
  });

  let payload = start.payload();

  assert!(!payload.contains("term_fallback"), "{payload}");
  assert_eq!(Event::decode("exec.start", SCHEMA_VERSION, &payload).unwrap(), start);
}

// Version 6 renamed `runtime` to `driver` without compatibility (the drivers
// design): rows outside the retained schema range are refused with the
// version instead of a payload error about a field.
#[test]
fn a_payload_of_another_version_is_refused() {
  assert_eq!(
    Event::decode("run.stop", SCHEMA_VERSION, r#"{"ended":false}"#).unwrap(),
    Event::RunStop(RunStop {
      ended: false,
      by: None,
      actor: None
    })
  );
  for version in [1, 9, SCHEMA_VERSION + 1] {
    assert!(matches!(Event::decode("run.stop", version, "{}"), Err(DecodeError::Version { .. })), "{version}");
  }
}

#[test]
fn a_principal_is_one_string_in_the_payload() {
  let stop = Event::RunStop(RunStop {
    ended: false,
    by: Some(Principal::Client("laptop".into())),
    actor: None,
  });
  assert_eq!(stop.payload(), r#"{"by":"client:laptop","ended":false}"#);
  assert!(matches!(Event::decode("run.stop", SCHEMA_VERSION, r#"{"by":"someone"}"#), Err(DecodeError::Payload { .. })));
}

// ROOT CAUSE:
//
// If a trace held events of an older schema, `needy report` said they were
// not one this build reads "(1 to 9)", because the message named a range
// starting at 1 while the build reads only the current version.
//
// Before the fix, the message contradicted itself for versions inside 1..9.
// The fix named the one version the build read. Schema 11 retains schema 10,
// so the message must now name that exact supported range.
#[test]
fn a_refused_schema_version_names_the_supported_range() {
  let error = Event::decode("run.stop", 9, "{}").unwrap_err().to_string();
  assert_eq!(error, format!("event schema version 9 is not one this build reads; it reads versions 10 through {SCHEMA_VERSION}"));
}

#[test]
fn stop_completion_requires_an_explicit_ownership_fact_in_schema_11() {
  assert!(matches!(Event::decode("run.stop", SCHEMA_VERSION, "{}"), Err(DecodeError::Payload { .. })));
  assert!(matches!(Event::decode("run.stop", 9, r#"{"actor":null}"#), Err(DecodeError::Version { .. })));
  let event = Event::RunStop(RunStop {
    ended: true,
    ..Default::default()
  });
  assert_eq!(Event::decode("run.stop", SCHEMA_VERSION, &event.payload()).unwrap(), event);
}

#[test]
fn schema_10_history_keeps_its_original_stop_and_event_meaning() {
  let stop = Event::decode("run.stop", 10, r#"{"by":"operator","actor":{"how":"graceful"}}"#).unwrap();
  assert_eq!(
    stop,
    Event::RunStop(RunStop {
      ended: false,
      by: Some(Principal::Operator),
      actor: Some(ActorStop::Graceful)
    })
  );
  assert_eq!(Event::decode("run.stop", 10, "{}").unwrap(), Event::RunStop(RunStop::default()));
  assert_eq!(Event::decode("trace.gap", 10, r#"{"dropped":6}"#).unwrap(), Event::TraceGap(TraceGap { dropped: 6 }));
}
