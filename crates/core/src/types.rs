//! Values the system produces and that producers and consumers share:
//! decisions, placements, run states, exec origins and injected faults.
//! Values users write live in `needy-config`.

use std::collections::{BTreeMap, BTreeSet};
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

/// How a runner can run an actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorRuntime {
  /// A Firecracker microVM: a usable KVM and the actor image.
  Firecracker,
  /// A container through a Docker daemon that answers. Which OCI runtimes
  /// (`runc`, gVisor's `runsc`) it has is checked when a run is created.
  // TODO(runner-actor): the capability does not carry the OCI runtimes; it
  // needs to once actors run on runners other than the control host's own,
  // which probes its Docker directly.
  Container,
  /// Experimental (proof of concept): a process on a Mac under a Seatbelt
  /// profile.
  // TODO(macos-native): no runner reports it, so a native actor runs on the
  // control host only (no `runner:` selector) while it is a proof of
  // concept; reporting it waits for the owner's productize decision.
  MacosNative,
}

impl ActorRuntime {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Firecracker => "firecracker",
      Self::Container => "container",
      Self::MacosNative => "macos-native",
    }
  }
}

impl fmt::Display for ActorRuntime {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

/// How a runner can create an executor. A profile's `provider:` names one;
/// the providers no runner reports (`containerd`, `firecracker`, `k8s`) are
/// refused when a run is created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutorProvider {
  /// The runner's own process space; every runner has it.
  Native,
  /// A container the runner's Docker daemon creates.
  Docker,
}

impl ExecutorProvider {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Native => "native",
      Self::Docker => "docker",
    }
  }
}

impl fmt::Display for ExecutorProvider {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

/// What a runner can host: each role with its runtimes or providers. It
/// holds a role when that role has at least one. Written as
/// `actor(firecracker) executor(docker,native)`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Capabilities {
  pub actor: BTreeSet<ActorRuntime>,
  pub executor: BTreeSet<ExecutorProvider>,
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
    // Each role's runtimes or providers by name, as the CLI lists them.
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

#[cfg(test)]
#[path = "types_test.rs"]
mod tests;
