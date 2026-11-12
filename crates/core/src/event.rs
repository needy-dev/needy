//! The events a run records: one [`EventKind`] and one serde payload per
//! kind. Producers build an [`Event`] and hand its kind and JSON
//! [`Event::raw`] payload to `needy-trace`; readers rebuild it with
//! [`Event::decode`].
//!
//! Payloads are plain JSON objects with `deny_unknown_fields`, so a field
//! that one side renames fails to decode instead of reading as empty.

use needy_config::profile::Fault;
use serde::{Deserialize, Serialize};

use crate::types::{Capture, Fired, Network, Origin, Outcome, Principal};

/// The version of the payload schema that producers write.
///
/// Version 2 added who caused an event (`by`: `run.create`, `run.stop`,
/// `exec.start`) and `exec.kill`; version 3 added `files.fail`; version 4
/// added `actor.start`'s `runtime`; version 5 added `run.create`'s
/// `recipe_time`; version 6 renamed `actor.start`'s `runtime` and
/// `run.create`'s executors' `runtime` to `driver`, with the drivers' names
/// (`linux-firecracker`, `linux-container (runc)`, `linux-container`);
/// version 7 added `actor.ready`; version 8 added `actor.start`'s `capture`;
/// version 9 gave `fault.injected` the fault's `name` and `domain` and its
/// effects their own fields (`exit`, `signal`, `refuse`, `status`, ...), and
/// added `chaos.add` and `chaos.remove`; version 10 added `run.stop`'s
/// `actor`, how a stop ended a running actor.
pub const SCHEMA_VERSION: u32 = 10;

/// The versions [`Event::decode`] reads. Rows of another version are
/// rejected.
// NOTICE: version 6 renamed two fields (`runtime` to `driver`) without
// compatibility (the drivers design,
// `docs/ai/references/architecture/2026-11-12-drivers-and-extension-points-design.md`,
// allows breaking wire changes), so traces written before it are rejected
// with a version error instead of decoding a renamed field as unknown.
// Version 7 only added a kind, but a reader of 6 would refuse it, so traces
// of 6 are refused here too: one version is read (no compatibility).
// Version 8 only added an optional field (`actor.start`'s `capture`); traces
// of 7 are refused all the same, by the same rule. Version 9 renamed
// `fault.injected`'s `kind` to `domain` with new values, so traces of 8 are
// refused as well. Version 10 only added an optional field (`run.stop`'s
// `actor`); traces of 9 are refused by the same rule.
// Version 0 is the flat string-attribute rows written before the typed
// schema; they are not read either (module layout redesign, principle 7).
pub const READ_VERSIONS: std::ops::RangeInclusive<u32> = SCHEMA_VERSION..=SCHEMA_VERSION;

/// Declares the kinds, their names on the wire and their payload types.
macro_rules! events {
  ($( $(#[$doc:meta])* $variant:ident($payload:ty) = $name:literal, )*) => {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub enum EventKind {
      $( $(#[$doc])* $variant, )*
    }

    impl EventKind {
      pub const ALL: &[EventKind] = &[$( EventKind::$variant, )*];

      pub fn as_str(self) -> &'static str {
        match self {
          $( EventKind::$variant => $name, )*
        }
      }
    }

    /// A typed event.
    #[derive(Debug, Clone, PartialEq)]
    pub enum Event {
      $( $(#[$doc])* $variant($payload), )*
    }

    impl Event {
      pub fn kind(&self) -> EventKind {
        match self {
          $( Event::$variant(_) => EventKind::$variant, )*
        }
      }

      /// The payload as the JSON object that is stored.
      pub fn payload(&self) -> String {
        // NOTICE: payloads are structs of strings, numbers, lists and
        // externally tagged enums, which always serialize to JSON.
        let json = match self {
          $( Event::$variant(payload) => serde_json::to_string(payload), )*
        };
        json.expect("event payloads serialize to JSON")
      }

      /// The payload as JSON for `needy-trace`.
      pub fn raw(&self) -> Box<serde_json::value::RawValue> {
        let json = match self {
          $( Event::$variant(payload) => serde_json::value::to_raw_value(payload), )*
        };
        // NOTICE: as in `payload`, these types always serialize.
        json.expect("event payloads serialize to JSON")
      }

      /// Rebuilds an event from its stored kind, schema version and payload.
      pub fn decode(kind: &str, version: u32, payload: &str) -> Result<Event, DecodeError> {
        if !READ_VERSIONS.contains(&version) {
          return Err(DecodeError::Version { found: version, expected: SCHEMA_VERSION });
        }
        let kind: EventKind = kind.parse()?;
        let wrap = |source| DecodeError::Payload { kind, source };
        Ok(match kind {
          $( EventKind::$variant => Event::$variant(serde_json::from_str(payload).map_err(wrap)?), )*
        })
      }
    }
  };
}

events! {
  RunCreate(RunCreate) = "run.create",
  RunStop(RunStop) = "run.stop",
  /// Creating the run failed; whatever it had acquired is released.
  RunFail(RunFail) = "run.fail",
  /// The actor's VM started.
  ActorStart(ActorStart) = "actor.start",
  /// The actor's VM process exited.
  ActorVmExit(ActorVmExit) = "actor.vm.exit",
  /// The actor's init fetched its configuration.
  ActorConfig(ActorConfig) = "actor.config",
  /// The actor's process service answered a request: its driving program's
  /// terminal can be attached.
  ActorReady(ActorReady) = "actor.ready",
  /// The actor's driving program exited.
  ActorExit(ActorExit) = "actor.exit",
  FilesCopyIn(FilesCopyIn) = "files.copy_in",
  FilesMirrorOut(FilesMirrorOut) = "files.mirror_out",
  /// A copy-in or mirror-out failed.
  FilesFail(FilesFail) = "files.fail",
  ExecDecide(ExecDecide) = "exec.decide",
  ExecStart(ExecStart) = "exec.start",
  ExecExit(ExecExit) = "exec.exit",
  /// A signal sent to a run's process through the control plane.
  ExecKill(ExecKill) = "exec.kill",
  /// A chaos fault fired; it carries the fault's name.
  FaultInjected(Fired) = "fault.injected",
  /// `needy chaos add` added a fault to the running run.
  ChaosAdd(ChaosAdd) = "chaos.add",
  /// `needy chaos rm` removed a fault from the running run.
  ChaosRemove(ChaosRemove) = "chaos.remove",
  NetRequest(NetRequest) = "net.request",
  NetConnect(NetConnect) = "net.connect",
  NetDeny(NetDeny) = "net.deny",
  NetError(NetError) = "net.error",
  /// The trace dropped events because its queue was full. `needy-trace`
  /// writes it.
  TraceGap(TraceGap) = "trace.gap",
  /// Per-hook latencies since the last summary. `needy-trace` writes it when
  /// the profile turns on `trace.timing`.
  TraceTiming(TraceTiming) = "trace.timing",
}

impl std::fmt::Display for EventKind {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(self.as_str())
  }
}

impl std::str::FromStr for EventKind {
  type Err = DecodeError;

  fn from_str(text: &str) -> Result<Self, Self::Err> {
    EventKind::ALL.iter().copied().find(|kind| kind.as_str() == text).ok_or_else(|| DecodeError::Kind(text.to_string()))
  }
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
  #[error("unknown event kind {0:?}")]
  Kind(String),
  #[error("event schema version {found} is not one this build reads; it reads version {expected} only")]
  Version { found: u32, expected: u32 },
  #[error("invalid {kind} payload")]
  Payload {
    kind: EventKind,
    #[source]
    source: serde_json::Error,
  },
}

/// An event with the time it was recorded.
#[derive(Debug, Clone, PartialEq)]
pub struct Recorded {
  pub time_ms: i64,
  pub event: Event,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCreate {
  pub name: String,
  pub group: String,
  /// The profile's name.
  pub profile: String,
  pub executors: Vec<ExecutorRef>,
  /// Who caused it; absent in version 1 traces and for what the control
  /// plane does on its own, such as stopping runs when it shuts down.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub by: Option<Principal>,
  /// The `{time}` the run's recipe was compiled with (`YYYYMMDD-HHMMSS`,
  /// UTC, when the run was created), which with the seed from the run's ID
  /// rebuilds the same machine on replay. `None` for a run without a recipe
  /// and in traces before version 5.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub recipe_time: Option<String>,
}

/// An executor instance a run uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutorRef {
  pub name: String,
  /// The executor driver that created the instance, or `runner` for a
  /// runner's own process space.
  pub driver: String,
  pub instance: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunStop {
  /// Who caused it; absent in version 1 traces and for what the control
  /// plane does on its own, such as stopping runs when it shuts down.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub by: Option<Principal>,
  /// How the stop ended the run's running actor; absent when the actor had
  /// ended already, or the run had none.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub actor: Option<ActorStop>,
}

/// How `run stop` ended a running actor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "how", deny_unknown_fields)]
pub enum ActorStop {
  /// Its driving program was signalled, the actor mirrored out and ended by
  /// itself.
  Graceful,
  /// The actor was killed, without its mirror-out; `reason` says why.
  Forced { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunFail {
  /// Why, with its causes.
  pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActorStart {
  pub pid: u32,
  pub vcpus: u32,
  pub mem_mib: u32,
  /// The actor driver that ran the actor, with what says how it was
  /// isolated where its name does not: `linux-firecracker`,
  /// `linux-container (runc)` or `linux-container (runsc)`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub driver: Option<String>,
  /// How the actor catches an exec, from its driver's facets; `None` in a
  /// trace from before schema version 7.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub capture: Option<Capture>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActorVmExit {
  pub code: Option<i32>,
  pub signal: Option<String>,
  /// Waiting for the VM failed.
  pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActorConfig {
  /// The command names the actor's kernel routes to needy; `None` routes
  /// every exec.
  pub route_names: Option<Vec<String>>,
  /// Mount points of the actor's workspace mounts.
  pub mounts: Vec<String>,
}

/// The daemon reached the actor's process service; see the daemon's
/// `actor::state` for what counts as ready.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActorReady {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActorExit {
  pub code: Option<i32>,
  pub signal: Option<String>,
}

/// One side of a file transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileEnd {
  Actor,
  Executor { name: String, path: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesCopyIn {
  /// The mapping's name.
  pub name: String,
  pub to: FileEnd,
  /// Size of the archive.
  pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesMirrorOut {
  /// The mapping's name.
  pub name: String,
  pub from: FileEnd,
  /// Size of the archive.
  pub bytes: u64,
  /// Entries unpacked, when the daemon unpacked them.
  pub entries: Option<u64>,
}

/// Which way a file transfer goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transfer {
  CopyIn,
  MirrorOut,
}

impl Transfer {
  /// The mode's name as a profile writes it.
  pub fn as_str(self) -> &'static str {
    match self {
      Self::CopyIn => "copy-in",
      Self::MirrorOut => "mirror-out",
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesFail {
  /// The mapping's name.
  pub name: String,
  pub transfer: Transfer,
  pub reason: String,
}

/// The `exec.decide` result for one command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecDecide {
  /// The command as asked.
  pub argv: Vec<String>,
  /// The command after launcher resolution.
  pub effective: Vec<String>,
  pub origin: Origin,
  /// Where it ran, or why it did not.
  pub outcome: Outcome,
  /// Indices of the rules that matched.
  pub rules: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecStart {
  pub process: String,
  /// The executor's name in the profile.
  pub executor: String,
  pub argv: Vec<String>,
  pub origin: Origin,
  /// The terminal type the executor used instead of the client's, which it
  /// has no terminfo entry for. Absent when kept.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub term_fallback: Option<TermFallback>,
  /// Who caused it; absent in version 1 traces and for what the control
  /// plane does on its own, such as stopping runs when it shuts down.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub by: Option<Principal>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TermFallback {
  pub requested: String,
  pub used: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecExit {
  pub process: String,
  pub executor: String,
  pub argv: Vec<String>,
  pub origin: Origin,
  pub code: Option<i32>,
  pub signal: Option<String>,
  /// Wall time from start to exit.
  pub ms: u64,
}

/// A signal sent to a run's process; its `exec.exit` follows when the
/// process ends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecKill {
  pub process: String,
  /// The executor's name in the profile.
  pub executor: String,
  /// As given, such as `TERM`; empty for the executor's default.
  pub signal: String,
  /// Who caused it; absent in version 1 traces and for what the control
  /// plane does on its own, such as stopping runs when it shuts down.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub by: Option<Principal>,
}

/// A fault added to a running run. A replay applies it at the same place
/// in the event sequence; its counts start at zero there.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChaosAdd {
  pub name: String,
  pub fault: Box<Fault>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub by: Option<Principal>,
}

/// A fault removed from a running run, whether the profile or
/// `needy chaos add` had added it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChaosRemove {
  pub name: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub by: Option<Principal>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetRequest {
  pub network: Network,
  pub host: String,
  pub path: String,
  /// The upstream's HTTP status.
  pub status: u16,
  pub ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetConnect {
  pub network: Network,
  pub host: String,
  /// `host:port` of the tunnel.
  pub authority: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetDeny {
  pub network: Network,
  pub host: String,
  /// The request path; `None` for a `CONNECT` tunnel.
  pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetError {
  pub network: Network,
  pub host: String,
  /// The request path; `None` for a `CONNECT` tunnel.
  pub path: Option<String>,
  pub error: String,
}

/// The payload `needy-trace` writes for `trace.gap`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceGap {
  /// Events lost at this point of the trace.
  pub dropped: u64,
}

/// The payload `needy-trace` writes for `trace.timing`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceTiming {
  /// By hook point, for example `exec.decide`.
  pub hooks: std::collections::BTreeMap<String, HookTiming>,
}

/// The latency samples of one hook point, in microseconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookTiming {
  pub count: u64,
  pub total_us: u64,
  pub min_us: u64,
  pub max_us: u64,
}

#[cfg(test)]
#[path = "event_test.rs"]
mod tests;
