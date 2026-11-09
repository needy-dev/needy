//! The events a session records: one [`EventKind`] and one serde payload per
//! kind. Producers build an [`Event`] and store its kind and JSON
//! [`Event::payload`]; readers rebuild it with [`Event::decode`].
//!
//! Payloads are plain JSON objects with `deny_unknown_fields`, so a field
//! that one side renames fails to decode instead of reading as empty.

use serde::{Deserialize, Serialize};

use crate::types::{Fired, Network, Origin, Outcome};

/// The version of the payload schema. Rows of another version are rejected.
// NOTICE: version 0 is the flat string-attribute rows written before this
// schema; they are not read (module layout redesign, principle 7).
pub const SCHEMA_VERSION: u32 = 1;

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

      /// Rebuilds an event from its stored kind, schema version and payload.
      pub fn decode(kind: &str, version: u32, payload: &str) -> Result<Event, DecodeError> {
        if version != SCHEMA_VERSION {
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
  SessionCreate(SessionCreate) = "session.create",
  SessionStop(SessionStop) = "session.stop",
  /// The actor's VM started.
  ActorStart(ActorStart) = "actor.start",
  /// The actor's VM process exited.
  ActorVmExit(ActorVmExit) = "actor.vm.exit",
  /// The actor's init fetched its configuration.
  ActorConfig(ActorConfig) = "actor.config",
  /// The actor's driving program exited.
  ActorExit(ActorExit) = "actor.exit",
  FilesCopyIn(FilesCopyIn) = "files.copy_in",
  FilesMirrorOut(FilesMirrorOut) = "files.mirror_out",
  ExecDecide(ExecDecide) = "exec.decide",
  ExecStart(ExecStart) = "exec.start",
  ExecExit(ExecExit) = "exec.exit",
  FaultInjected(Fired) = "fault.injected",
  NetRequest(NetRequest) = "net.request",
  NetConnect(NetConnect) = "net.connect",
  NetDeny(NetDeny) = "net.deny",
  NetError(NetError) = "net.error",
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
  #[error("event schema version {found} is not {expected}")]
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
pub struct SessionCreate {
  pub name: String,
  pub group: String,
  /// The profile's name.
  pub profile: String,
  pub executors: Vec<ExecutorRef>,
}

/// An executor instance a session uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutorRef {
  pub name: String,
  pub runtime: String,
  pub instance: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionStop {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActorStart {
  pub pid: u32,
  pub vcpus: u32,
  pub mem_mib: u32,
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
  /// The asciicast recording of a terminal session.
  // TODO(trace): terminal recording is removed in slice 4, and this field
  // with it.
  pub cast: Option<String>,
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

#[cfg(test)]
#[path = "event_test.rs"]
mod tests;
