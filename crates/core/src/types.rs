//! Values the system produces and that producers and consumers share:
//! decisions, placements, session states, exec origins and injected faults.
//! Values users write live in `needy-config`.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use needy_config::profile::{Fault, FaultKind};
use serde::{Deserialize, Serialize};

/// Where a command runs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
  Actor,
  Executor(String),
}

impl fmt::Display for Placement {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Actor => f.write_str("actor"),
      Self::Executor(name) => write!(f, "executor {name}"),
    }
  }
}

/// What happens to a command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
  Run(Placement),
  /// A rule refused it, with the rule's message.
  Deny(String),
  /// An injected fault failed it with this errno.
  Fault(String),
}

/// The `exec.decide` result: where a command runs and how it is rewritten.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
  pub outcome: Outcome,
  /// Arguments to run, after prepend, append, wrap and exec.
  pub argv: Vec<String>,
  /// Final environment after the rules' changes to the context's environment.
  pub env: BTreeMap<String, String>,
  /// The process should start from `env` alone, not the executor's environment.
  pub clear_env: bool,
  pub cwd: String,
  pub files: Vec<String>,
  /// Faults the matching rules name.
  pub faults: Vec<Fault>,
  pub timeout: Option<Duration>,
  /// Indices of the rules that matched, in order.
  pub matched: Vec<usize>,
  /// The command after launcher resolution; rules match against this.
  pub effective: Vec<String>,
  /// Faults that chaos injected into this command. The flow applies their
  /// delays; an errno is already in `outcome`.
  pub fired: Vec<Fired>,
}

/// A session's life cycle. The first terminal state wins.
// TODO(daemon): `Creating` and `Stopping` are not entered yet; slice 8 moves
// the transitions into `daemon::session::state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
  Creating,
  Running,
  Stopping,
  Stopped,
  /// The actor's driving program exited.
  Finished,
  Failed,
}

impl SessionState {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Creating => "creating",
      Self::Running => "running",
      Self::Stopping => "stopping",
      Self::Stopped => "stopped",
      Self::Finished => "finished",
      Self::Failed => "failed",
    }
  }
}

impl fmt::Display for SessionState {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

/// Who started an exec. On the wire it is the `needy.v1.Origin` enum;
/// `needy_api_server::protocol::domain` maps between the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
  /// `needy exec`.
  Cli,
  /// `needy fs`.
  Fs,
  /// The daemon's own copy-in and mirror-out.
  Files,
  /// A command the actor's kernel routed through the proxy.
  ActorProxy,
  /// The SFTP server behind an actor's workspace mount.
  ActorMount,
}

/// The network a request came from, at a gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Network {
  Actor,
  // TODO(executor-network): executors have no gateway yet; the variant is
  // reserved for the executor network the CLI note describes and waits for
  // the owner to name that slice. Only the actor gateway exists.
  Executor,
}

/// The `net.request` or `net.connect` result: whether a gateway lets a
/// request or tunnel through, and what it adds.
// NOTICE: `headers` may hold an injected credential; do not log a decision.
#[derive(Clone, PartialEq, Eq)]
pub struct NetDecision {
  pub outcome: NetOutcome,
  /// Headers set on the upstream request, as name and value. A `net.connect`
  /// tunnel is opaque, so nothing adds headers to it.
  pub headers: Vec<(String, String)>,
  /// Faults that chaos injected. The flow applies their delays; an errno is
  /// already in `outcome`.
  pub fired: Vec<Fired>,
}

impl NetDecision {
  /// Allowed and unchanged, before any interceptor.
  pub fn allowed() -> NetDecision {
    NetDecision {
      outcome: NetOutcome::Allow,
      headers: Vec::new(),
      fired: Vec::new(),
    }
  }
}

impl fmt::Debug for NetDecision {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let names: Vec<&str> = self.headers.iter().map(|(name, _)| name.as_str()).collect();
    f.debug_struct("NetDecision").field("outcome", &self.outcome).field("headers", &names).field("fired", &self.fired).finish()
  }
}

/// What happens to a request or tunnel at a gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetOutcome {
  Allow,
  /// The network policy does not allow the host.
  Deny,
  /// An injected fault failed it with this errno.
  Fault(String),
}

/// A fault that chaos injected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fired {
  pub kind: FaultKind,
  /// What it hit: the command as typed, the host, the path under the mount,
  /// or the sensor chip.
  pub target: String,
  pub effect: Effect,
}

/// What an injected fault did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Effect {
  /// The operation failed with this errno.
  pub errno: Option<String>,
  /// The operation was held this long first.
  #[serde(with = "millis")]
  pub delay: Option<Duration>,
  /// A sensor read this temperature, in millidegrees Celsius.
  pub temp_mc: Option<i32>,
}

/// Durations as whole milliseconds on the wire.
mod millis {
  use std::time::Duration;

  use serde::{Deserialize, Deserializer, Serializer};

  pub fn serialize<S: Serializer>(value: &Option<Duration>, s: S) -> Result<S::Ok, S::Error> {
    match value {
      Some(d) => s.serialize_some(&(d.as_millis() as u64)),
      None => s.serialize_none(),
    }
  }

  pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
    Ok(Option::<u64>::deserialize(d)?.map(Duration::from_millis))
  }
}
