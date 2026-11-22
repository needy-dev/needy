//! Domain types and trait for `RunService`.

use std::collections::{BTreeMap, HashMap};
use std::pin::Pin;

use needy_common::process::Exit;
use needy_core::event::{ActorStop, ExecutorRef};
use needy_core::intercept::exec::ExecContext;
use needy_core::types::{ActorState, Principal, RunState};
use tokio_stream::Stream;

use super::{ControlError, Diagnostic, Result, RunnerHost, WindowSize};

/// A run to create from a profile.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct NewRun {
  /// The profile, self-contained: no `extends` to read, host paths
  /// absolute.
  pub profile_yaml: String,
  /// The actor's recipe when the profile names a recipe file; empty for a
  /// built-in recipe or none.
  pub recipe_yaml: String,
  /// The profile's name, derived by the client from its file name.
  pub profile_name: String,
  /// Empty for the run's ID.
  pub group: String,
  /// Empty for the run's ID.
  pub name: String,
  /// Appended to the profile's driving program (`actor.command`).
  pub actor_args: Vec<String>,
  /// Who asks for the run.
  pub by: Option<Principal>,
  /// Run the driving program on a terminal of this size.
  pub tty: Option<WindowSize>,
  /// Start only after the first terminal client subscribed (`run create -it`).
  pub wait_for_attach: bool,
  /// One-use execution capability supplied by the creating client.
  pub client_execution: String,
  /// The directory `needy run create` runs in, on the creating client's
  /// machine: the workspace of a profile that names none. Empty when the
  /// client sent none.
  pub client_dir: String,
  /// The creating client runs on the control host: its request arrived on
  /// one of the control host's Unix sockets. The adapter sets it; the wire
  /// does not say.
  pub on_control_host: bool,
  /// Optional commit layer from the image's lockfile, stacked on top of the
  /// image definition's built layers.
  pub image_commit: Option<needy_core::image::Commit>,
  /// Pinned feature digests (`canonical_ref -> sha256:<hex>`) from the
  /// image's lockfile.
  pub pinned_features: BTreeMap<String, String>,
}

impl std::fmt::Debug for NewRun {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("NewRun")
      .field("profile_name", &self.profile_name)
      .field("name", &self.name)
      .field("group", &self.group)
      .field("client_execution", &(!self.client_execution.is_empty()).then_some("<redacted>"))
      .finish_non_exhaustive()
  }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunInfo {
  pub id: String,
  pub name: String,
  pub group: String,
  pub state: RunState,
  /// The executors the run holds.
  pub executors: Vec<ExecutorRef>,
  /// The executors the run held and gave back when it stopped,
  /// finished or failed.
  pub released: Vec<ExecutorRef>,
  /// The executors no runner reported after the control plane restarted:
  /// whether they still run is unknown.
  pub lost: Vec<ExecutorRef>,
  pub created_ms: i64,
  pub profile: String,
  /// How the actor's driving program ended; `None` while it runs, without
  /// an actor, or when the VM ended before the program reported its exit.
  pub actor_exit: Option<Exit>,
  /// The driving program runs on a terminal.
  pub tty: bool,
  /// Why the run failed, with its causes.
  pub failure: Option<String>,
  /// Where its actor stands; `None` before the actor starts, and for a run
  /// listed from `state.db` after it ended.
  pub actor: Option<ActorState>,
  /// The actor's workspace; `None` when it has none, and for a run listed
  /// from `state.db` after it ended.
  pub workspace: Option<RunWorkspace>,
  /// The digest (`sha256:<hex>`) of the image snapshot the actor booted;
  /// `None` when booting the base image alone or an OCI reference.
  pub snapshot: Option<String>,
  /// True when the lockfile's commit layer was applied on a changed base.
  pub commit_rebased: bool,
  /// Resolved feature digests (`canonical_ref -> sha256:<hex>`) for the CLI
  /// to record in the image's lockfile.
  pub resolved_features: BTreeMap<String, String>,
}

/// Where a run's workspace is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunWorkspace {
  /// The directory in the actor.
  pub path: String,
  /// The executor whose directory the actor sees live; empty when the
  /// files are copied in from the control host, or for a fresh directory.
  pub executor: String,
  /// Where the files come from: a directory on the executor's machine, or
  /// on the control host when `executor` is empty; empty for a fresh
  /// directory.
  pub source: String,
  pub writable: bool,
}

/// Where a routed command runs and how it is rewritten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
  pub target: Target,
  pub argv: Vec<String>,
  /// The variables the decision sets or changes.
  pub env: HashMap<String, String>,
  pub clear_env: bool,
  pub cwd: String,
  /// Run the program of the same name on the executor's PATH when `argv[0]`
  /// is an absolute path the executor does not have.
  pub search_path_if_missing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
  /// The actor runs the command itself.
  Actor,
  Executor(String),
  /// A rule refused the command; the message is what the proxy prints.
  Refused(String),
  /// A chaos fault ends the command in place of running it.
  Fault(ExecFault),
}

/// How a chaos fault ends a command in place of running it: by `exit`,
/// `signal` or `hang`, after writing `stderr`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecFault {
  pub name: String,
  pub exit: Option<i32>,
  pub signal: Option<String>,
  pub hang: bool,
  pub stderr: String,
}

/// A fault `needy chaos add` added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultAdded {
  /// The full run ID.
  pub run: String,
  /// What the fault cannot reach.
  // NOTICE: empty since 2026-11-13, when the daemon began to send a running
  // actor its new route list; it stays for the next fault a run cannot apply
  // everywhere (such as a sensor fault at run time, `TODO(chaos-sensor)`).
  pub warnings: Vec<String>,
}

/// A run's chaos faults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunFaults {
  pub run: String,
  pub faults: Vec<FaultInfo>,
}

/// One chaos fault of a run, with its counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultInfo {
  pub name: String,
  pub domain: String,
  /// The fault as YAML, as a profile's `chaos.faults` entry.
  pub fault_yaml: String,
  pub source: needy_core::chaos::FaultSource,
  pub matched: u64,
  pub fired: u64,
}

/// A profile to check against the runners it would run on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileToCheck {
  /// As in [`NewRun`].
  pub profile_yaml: String,
  pub recipe_yaml: String,
}

/// Where a profile would run, and what the control plane found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileCheck {
  /// Where the actor would run; `None` when its selector does not resolve.
  pub actor: Option<Placement>,
  /// Where each executor would run; one whose selector does not resolve is
  /// missing.
  pub executors: BTreeMap<String, Placement>,
  /// The model name of the CPU the recipe was checked against; empty when
  /// unknown.
  pub actor_cpu: String,
  pub findings: Vec<ProfileFinding>,
  /// The image the actor would start from, as its driver names it; empty
  /// when unknown.
  pub actor_image: String,
  /// The base ABI the actor image records, when it is this control plane's;
  /// `None` when it is not, which a finding explains.
  pub actor_base_abi: Option<u32>,
}

/// The host an actor or executor would run on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
  /// The control host itself.
  ControlHost,
  Runner(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileFinding {
  pub diagnostic: Option<Diagnostic>,
  pub error: bool,
  pub message: String,
}

/// A run's actor console: chunks as they are read.
pub type Logs = Pin<Box<dyn Stream<Item = Result<Vec<u8>>> + Send>>;

/// One trace event as stored: its payload is JSON of `kind`'s schema at
/// `version`, which clients decode with `needy_core::event`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEvent {
  pub seq: u64,
  pub time_ms: i64,
  pub kind: String,
  pub version: u32,
  pub payload: String,
}

/// A run's trace events, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunEvents {
  /// The full run ID.
  pub run: String,
  pub events: Vec<StoredEvent>,
}

/// How `run stop` ends a running actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopMode {
  /// The driving program gets SIGTERM, then SIGKILL after `grace`; the
  /// actor mirrors out and ends by itself before the stop goes on.
  Graceful { grace: std::time::Duration },
  /// The actor is killed at once, without its mirror-out.
  Now,
}

/// The grace period a stop gives a driving program when the caller names
/// none.
pub const STOP_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

impl Default for StopMode {
  fn default() -> Self {
    Self::Graceful { grace: STOP_GRACE }
  }
}

/// A reserved private execution holder, usable by exactly one creation.
pub struct ClientExecution {
  pub run: String,
  pub holder: String,
  pub capability: String,
}

#[async_trait::async_trait]
pub trait Runs: Send + Sync + 'static {
  /// Reserve an execution holder using the creating client's admitted access.
  async fn prepare_client_execution(&self, _host: RunnerHost) -> Result<ClientExecution> {
    Err(ControlError::Unavailable("client execution is unavailable on this service".into()))
  }
  async fn create(&self, request: NewRun) -> Result<RunInfo>;
  /// A running, finished, stopped or failed run by ID.
  async fn get(&self, id: &str) -> Result<RunInfo>;
  async fn list(&self) -> Result<Vec<RunInfo>>;
  /// Stops a run; returns how it ended its running actor, `None` when the
  /// run had none.
  async fn stop(&self, id: &str, by: Principal, mode: StopMode) -> Result<Option<ActorStop>>;
  /// The actor's console output so far, and with `follow` what it writes
  /// next, until the run ends.
  async fn logs(&self, id: &str, follow: bool) -> Result<Logs>;
  /// The trace events of `run`: an ID, an ID prefix or `latest`.
  async fn events(&self, run: &str) -> Result<RunEvents>;
  /// Places a command the actor's proxy routed.
  async fn decide(&self, run: &str, ctx: ExecContext) -> Result<Route>;
  /// Checks a profile against the runners it would run on; creates nothing.
  async fn check(&self, profile: ProfileToCheck) -> Result<ProfileCheck>;
  /// Adds the fault `fault_yaml` (a `chaos.faults` entry) named `name` to
  /// live run `run`: an ID, an ID prefix, a name or `latest`.
  async fn add_fault(&self, run: &str, name: &str, fault_yaml: &str, by: Principal) -> Result<FaultAdded>;
  /// The chaos faults of run `run`.
  async fn faults(&self, run: &str) -> Result<RunFaults>;
  /// Removes the fault `name` from live run `run`; returns the full run ID.
  async fn remove_fault(&self, run: &str, name: &str, by: Principal) -> Result<String>;
}
