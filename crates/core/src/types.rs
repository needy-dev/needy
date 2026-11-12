//! Values the system produces and that producers and consumers share:
//! placements and outcomes, run states, exec origins, captures and injected
//! faults. Values users write live in `needy-config`; the decisions of each
//! interception domain live with the domain in [`crate::intercept`].

use std::collections::BTreeSet;
use std::fmt;
use std::time::Duration;

use needy_config::profile::{ActorDriver, ExecutorDriver, FaultDomain};
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
  /// The chaos fault of this name ended it without running; its effect is
  /// in the decision's `fault.injected`.
  Fault(String),
}

/// How an exec inside an actor is caught and handed to its proxy: an actor
/// driver's `Facets::capture` (`needy_core_actor::driver`), recorded in
/// `actor.start` so the report can say what the capture leaves out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capture {
  /// The guest kernel's `binfmt_needy`: every exec, by any path.
  Kernel,
  /// Shims made when the container image was built: a program created at
  /// run time and run directly is not caught (`profile check` warns).
  Shims,
  // TODO(macos-native): `FencedShims` (shims in a Seatbelt fence) comes with
  // the `macos-native` driver, if the owner productizes it.
}

/// A run's life cycle. The first terminal state wins. The daemon's
/// `run::state` holds the transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
  Creating,
  Running,
  Stopping,
  Stopped,
  /// The actor's driving program exited.
  Finished,
  Failed,
}

impl RunState {
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

impl fmt::Display for RunState {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

/// An actor's life cycle, which the daemon's `actor::state` advances the
/// same way for every actor driver. `Exited` is terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorState {
  /// Its driver is starting it, or started it and its guest boots.
  Starting,
  /// Its guest fetched the run's configuration.
  Configured,
  /// Its process service answered a request: a client can attach.
  Ready,
  /// It exited, or its run stopped it.
  Exited,
}

impl ActorState {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Starting => "starting",
      Self::Configured => "configured",
      Self::Ready => "ready",
      Self::Exited => "exited",
    }
  }
}

impl fmt::Display for ActorState {
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

impl Origin {
  /// The origin's name in traces and listings.
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Cli => "cli",
      Self::Fs => "fs",
      Self::Files => "files",
      Self::ActorProxy => "actor-proxy",
      Self::ActorMount => "actor-mount",
    }
  }

  /// Whether needy started the process for its own work (file transfers and
  /// workspace mounts) rather than to run a user's or the actor's command.
  pub fn is_internal(self) -> bool {
    matches!(self, Self::Fs | Self::Files | Self::ActorMount)
  }
}

impl fmt::Display for Origin {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

/// What a runner can host. On the wire it is the `needy.v1.RunnerRole`
/// enum; `needy_api_server::protocol::domain` maps between the two. A
/// runner's roles are those its [`Capabilities`] name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerRole {
  /// Hosts actors.
  Actor,
  /// Hosts executors.
  Executor,
}

impl RunnerRole {
  pub const ALL: [Self; 2] = [Self::Actor, Self::Executor];

  pub fn as_str(self) -> &'static str {
    match self {
      Self::Actor => "actor",
      Self::Executor => "executor",
    }
  }
}

impl fmt::Display for RunnerRole {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

impl std::str::FromStr for RunnerRole {
  type Err = UnknownRole;

  fn from_str(text: &str) -> Result<Self, UnknownRole> {
    Self::ALL.into_iter().find(|role| role.as_str() == text).ok_or_else(|| UnknownRole(text.to_string()))
  }
}

/// A role name this build does not know.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown runner role {0:?}; known roles are actor and executor")]
pub struct UnknownRole(pub String);

/// What a runner can host: each role with its drivers, as a profile names
/// them (`needy_config::profile::ActorDriver`, `ExecutorDriver`). It holds a
/// role when that role has at least one. Written as
/// `actor(linux-firecracker) executor(linux-container,native)`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Capabilities {
  pub actor: BTreeSet<ActorDriver>,
  pub executor: BTreeSet<ExecutorDriver>,
}

impl Capabilities {
  /// The roles it holds.
  pub fn roles(&self) -> BTreeSet<RunnerRole> {
    let mut roles = BTreeSet::new();
    if !self.actor.is_empty() {
      roles.insert(RunnerRole::Actor);
    }
    if !self.executor.is_empty() {
      roles.insert(RunnerRole::Executor);
    }
    roles
  }

  /// Only those of `roles`.
  pub fn within(&self, roles: &BTreeSet<RunnerRole>) -> Self {
    let keep = |role| roles.contains(&role);
    Self {
      actor: if keep(RunnerRole::Actor) {
        self.actor.clone()
      } else {
        BTreeSet::new()
      },
      executor: if keep(RunnerRole::Executor) {
        self.executor.clone()
      } else {
        BTreeSet::new()
      },
    }
  }

  pub fn is_empty(&self) -> bool {
    self.actor.is_empty() && self.executor.is_empty()
  }
}

impl fmt::Display for Capabilities {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    // Each role's drivers by name, as the CLI lists them.
    fn role<T: fmt::Display>(role: RunnerRole, kinds: &BTreeSet<T>) -> Option<String> {
      let mut names: Vec<String> = kinds.iter().map(T::to_string).collect();
      names.sort();
      (!names.is_empty()).then(|| format!("{role}({})", names.join(",")))
    }
    let roles: Vec<String> = [
      role(RunnerRole::Actor, &self.actor),
      role(RunnerRole::Executor, &self.executor),
    ]
    .into_iter()
    .flatten()
    .collect();
    if roles.is_empty() {
      return f.write_str("none");
    }
    f.write_str(&roles.join(" "))
  }
}

/// Who caused something the trace records: a run created or stopped, a
/// command started, a signal sent. Written as `operator` (the control
/// daemon's token or its socket), `client:NAME` (a paired client's token),
/// `runner:NAME` (a runner's credential) or `actor:RUN` (an actor, through
/// its own guest channel).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Principal {
  Operator,
  Client(String),
  Runner(String),
  Actor(String),
}

impl fmt::Display for Principal {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Operator => f.write_str("operator"),
      Self::Client(name) => write!(f, "client:{name}"),
      Self::Runner(name) => write!(f, "runner:{name}"),
      Self::Actor(run) => write!(f, "actor:{run}"),
    }
  }
}

impl std::str::FromStr for Principal {
  type Err = UnknownPrincipal;

  fn from_str(text: &str) -> Result<Self, UnknownPrincipal> {
    let unknown = || UnknownPrincipal(text.to_string());
    if text == "operator" {
      return Ok(Self::Operator);
    }
    let (kind, name) = text.split_once(':').ok_or_else(unknown)?;
    if name.is_empty() {
      return Err(unknown());
    }
    match kind {
      "client" => Ok(Self::Client(name.to_string())),
      "runner" => Ok(Self::Runner(name.to_string())),
      "actor" => Ok(Self::Actor(name.to_string())),
      _ => Err(unknown()),
    }
  }
}

// A principal is one string in a trace payload, as `Display` writes it.
impl Serialize for Principal {
  fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(self)
  }
}

impl<'de> Deserialize<'de> for Principal {
  fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    let text = String::deserialize(deserializer)?;
    text.parse().map_err(serde::de::Error::custom)
  }
}

/// A principal this build cannot read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown principal {0:?}; a principal is operator, client:NAME, runner:NAME or actor:RUN")]
pub struct UnknownPrincipal(pub String);

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

/// A fault that chaos injected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fired {
  /// The fault's name in the profile, or as `needy chaos add` named it.
  pub name: String,
  pub domain: FaultDomain,
  /// What it hit: the command as typed, the path in the actor, the host, or
  /// the sensor chip.
  pub target: String,
  pub effect: Effect,
}

/// What an injected fault did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Effect {
  /// The operation was held this long first.
  #[serde(with = "millis", skip_serializing_if = "Option::is_none")]
  pub delay: Option<Duration>,
  /// exec: the command exited with this code without running.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub exit: Option<i32>,
  /// exec: the command died of this signal without running.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub signal: Option<String>,
  /// exec: written to the command's stderr.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub stderr: Option<String>,
  /// exec and net: the operation never completed.
  #[serde(skip_serializing_if = "std::ops::Not::not")]
  pub hang: bool,
  /// file: the open failed with this errno.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub errno: Option<String>,
  /// net: the connection was refused.
  #[serde(skip_serializing_if = "std::ops::Not::not")]
  pub refuse: bool,
  /// net: the connection was closed without an answer.
  #[serde(skip_serializing_if = "std::ops::Not::not")]
  pub reset: bool,
  /// net: the request was answered with this HTTP status.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub status: Option<u16>,
  /// sensor: the sensors read this temperature, in millidegrees Celsius.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub temp_mc: Option<i32>,
}

impl Effect {
  /// Whether the operation does not go on after this effect: everything
  /// but a delay alone ends it.
  pub fn ends(&self) -> bool {
    self.exit.is_some() || self.signal.is_some() || self.hang || self.errno.is_some() || self.refuse || self.reset || self.status.is_some()
  }
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

#[cfg(test)]
#[path = "types_test.rs"]
mod tests;
