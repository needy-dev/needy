//! What the daemon implements: the domain traits behind the gRPC services,
//! who is calling ([`Caller`]), and the errors ([`ControlError`]) that
//! `protocol::status` maps to gRPC statuses in one table.
//!
//! Nothing here is generated code, except the messages of a runner's own
//! streams (see [`Runners::control`]). The adapters in `protocol::grpc`
//! convert requests into these types, call the traits, and convert the
//! results back.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::pin::Pin;

use needy_common::process::Exit;
use needy_core::event::{ActorStop, Event, ExecutorRef};
use needy_core::intercept::exec::ExecContext;
use needy_core::types::{ActorState, Capabilities, Origin, Principal, RunState, RunnerRole};
use tokio_stream::Stream;

/// Who is calling: the listener the call arrived on decides, unless the
/// middleware identified the caller by its bearer token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
  /// A client of the control socket, or one holding the daemon's own
  /// token: the bootstrap and admin credential.
  Operator,
  /// A named paired client with server-granted access. The trace names
  /// it `client:NAME`, including when it has administrator access.
  Client {
    id: String,
    name: String,
    access: ClientAccess,
  },
  /// An actor, on the listener bound to its run. Its calls act on that
  /// run; it may not create, stop or read the logs of runs.
  Actor { run: String },
  /// A runner, by the credential it presented on the control listener. It
  /// reaches `RunnerService.Heartbeat`, `Control` and `Task` only.
  Runner { name: String },
}

impl Caller {
  /// Who the trace says caused what this caller asks for.
  pub fn principal(&self) -> Principal {
    match self {
      Self::Operator => Principal::Operator,
      Self::Client { name, .. } => Principal::Client(name.clone()),
      Self::Actor { run } => Principal::Actor(run.clone()),
      Self::Runner { name } => Principal::Runner(name.clone()),
    }
  }

  /// Whether the caller has an operator's rights over runs and processes:
  /// the operator itself, or a paired client.
  pub fn is_operator(&self) -> bool {
    matches!(self, Self::Operator | Self::Client { .. })
  }
}

/// Safe producer facts. The protocol adapters validate and bound their wire form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
  pub reason: String,
  pub facts: BTreeMap<String, String>,
}

/// Why the daemon refused or failed a call. Each variant is one row of the
/// status table in `protocol::status`.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ControlError {
  /// Original domain cause with safe producer facts.
  #[error("{error}")]
  Diagnostic {
    #[source]
    error: Box<ControlError>,
    diagnostic: Diagnostic,
  },
  /// The caller may not do this, or a rule or an injected fault refused a
  /// command. `events` are the refused command's trace events: the faults
  /// chaos injected and the placement decision.
  #[error("{message}")]
  Denied { message: String, events: Vec<Event> },
  /// A secret in the request, such as a join token, is not valid. A
  /// missing or wrong bearer token is refused by the middleware instead.
  #[error("{0}")]
  Unauthenticated(String),
  #[error("{0}")]
  NotFound(String),
  /// The request is malformed.
  #[error("{0}")]
  Invalid(String),
  /// The request cannot be carried out, such as a run whose profile is
  /// invalid or a command that cannot start.
  #[error("{0}")]
  Failed(String),
  /// A limit is reached, such as a full executor with `on_full: reject`.
  #[error("{0}")]
  Exhausted(String),
  /// Something the call needs is going away, such as a closing executor.
  #[error("{0}")]
  Unavailable(String),
  #[error("{0}")]
  Internal(String),
}

impl ControlError {
  /// Attaches a known producer reason without changing the original cause.
  pub fn with_diagnostic(self, reason: &str, facts: impl IntoIterator<Item = (String, String)>) -> Self {
    Self::Diagnostic {
      error: Box::new(self),
      diagnostic: Diagnostic {
        reason: reason.into(),
        facts: facts.into_iter().collect(),
      },
    }
  }

  /// A known caller/protocol boundary refusal.
  pub fn role_denied(message: impl Into<String>) -> Self {
    Self::denied(message).with_diagnostic("auth.role_denied", [])
  }

  /// A refusal that carries no trace events.
  pub fn denied(message: impl Into<String>) -> Self {
    Self::Denied {
      message: message.into(),
      events: Vec::new(),
    }
  }
}

pub type Result<T, E = ControlError> = std::result::Result<T, E>;

/// A stream the daemon reads from a caller, or writes to one.
pub type Inputs = Pin<Box<dyn Stream<Item = ExecInput> + Send>>;
pub type Outputs = Pin<Box<dyn Stream<Item = Result<ExecOutput>> + Send>>;

// ------------------------------------------------------------- runs

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

// ------------------------------------------------------------ processes

/// The first request of an exec stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecOpen {
  Start(ExecSpec),
  /// `window` is the attaching client's terminal size, if it has one; a
  /// `read_only` client only watches.
  /// An empty `process` with a `run` attaches to the run's driving
  /// program.
  Attach {
    process: String,
    run: String,
    replay: bool,
    window: Option<WindowSize>,
    read_only: bool,
  },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecSpec {
  pub argv: Vec<String>,
  pub cwd: String,
  pub env: HashMap<String, String>,
  pub clear_env: bool,
  /// A terminal of this size; pipes when `None`.
  pub tty: Option<WindowSize>,
  pub detach: bool,
  /// Empty outside a run.
  pub run: String,
  /// Empty to let the run's rules decide.
  pub executor: String,
  /// A runner's own machine, outside any run; empty otherwise.
  pub runner: String,
  pub no_stdin: bool,
  pub origin: Origin,
  /// Run the program of the same name on the executor's PATH when `argv[0]`
  /// is an absolute path the executor does not have. Set by routing only.
  pub search_path_if_missing: bool,
  /// Who asks for the command; `None` for the control plane's own, and on
  /// the wire to an executor, which records no trace.
  pub by: Option<Principal>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowSize {
  pub rows: u32,
  pub cols: u32,
}

/// What a caller sends a running process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecInput {
  Stdin(Vec<u8>),
  Eof,
  Signal(String),
  Resize(WindowSize),
  /// The caller leaves without ending the process.
  Detach,
}

/// What an exec stream carries to its caller.
#[derive(Debug, Clone, PartialEq)]
pub enum ExecOutput {
  /// A trace event of the command's placement, before it starts.
  Trace(Event),
  Started(Started),
  Stdout(Vec<u8>),
  Stderr(Vec<u8>),
  Exited(Exit),
  /// The executor reported an error instead of an exit.
  Error(String),
  /// An in-band error with bounded safe producer facts.
  DiagnosticError {
    message: String,
    diagnostic: Diagnostic,
  },
  /// The daemon ends the stream before the process exited, for this
  /// reason; nothing follows.
  Ended(String),
  /// The executor took the caller's [`ExecInput::Detach`].
  Detached,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
  pub process: String,
  pub executor: String,
  pub pid: i64,
  pub argv: Vec<String>,
  pub cwd: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
  pub process: String,
  pub argv: Vec<String>,
  pub executor: String,
  pub pid: i64,
  pub running: bool,
  pub exit: Option<Exit>,
  pub tty: bool,
  pub started_ms: i64,
  pub run: String,
  /// Who started the process; `None` for a process the daemon did not start.
  pub origin: Option<Origin>,
}

#[async_trait::async_trait]
pub trait Processes: Send + Sync + 'static {
  /// Starts or attaches to a process; `input` is the rest of the caller's
  /// stream.
  async fn exec(&self, open: ExecOpen, input: Inputs) -> Result<Outputs>;
  /// The processes of `run`, or of the runner `runner`, or of every
  /// executor and connected runner when both are empty.
  async fn list(&self, run: &str, runner: &str) -> Result<Vec<ProcessInfo>>;
  async fn kill(&self, process: &str, signal: &str, by: Principal) -> Result<()>;
}

// ---------------------------------------------------------------- guest

/// The actor's configuration, which its init asks for at boot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuestConfig {
  pub run: String,
  pub user: GuestUser,
  /// Command names the kernel routes; `*` routes every exec.
  pub route_names: Vec<String>,
  pub hostname: String,
  pub uptime_offset_secs: i64,
  pub mounts: Vec<GuestMount>,
  /// The driving program.
  pub command: Vec<String>,
  pub env: HashMap<String, String>,
  pub cwd: String,
  pub gateway_port: u32,
  pub sensors: Vec<GuestSensor>,
  pub copy_in: Vec<GuestCopy>,
  pub mirror_out: Vec<GuestCopy>,
  pub sensor_faults: Vec<GuestFault>,
  /// The driving program's terminal; `None` for none.
  pub tty: Option<WindowSize>,
  /// The vsock port the guest serves `ProcessService` on when the driving
  /// program has a terminal.
  pub process_port: u32,
  /// Subscribe the first terminal client before starting the program.
  pub wait_for_attach: bool,
  /// The user and group the daemon runs as, who connect to the process
  /// service a container actor's init binds on a Unix socket.
  pub daemon_uid: u32,
  pub daemon_gid: u32,
  /// The digests (`sha256:<hex>`) of the layers init stacks over the base,
  /// lowest first; each layer disk's marker names one. Empty boots the
  /// base alone.
  pub layers: Vec<String>,
  /// The image's `PATH`, kept out of [`GuestConfig::env`]: it goes between
  /// the trampolines and the route stubs rather than replacing the driving
  /// program's `PATH`. Empty for none.
  pub image_path: String,
  pub session: GuestSession,
  /// Shell commands run in order as the run's user in `cwd` after setup and
  /// routing are enabled, before the driving program starts. Empty for build
  /// and edit sessions.
  pub start_hooks: Vec<String>,
  /// Shell commands run in order as the run's user in `cwd` after the driving
  /// program exits (or on graceful stop), before mirror-out and sync merges.
  /// Empty for build and edit sessions.
  pub stop_hooks: Vec<String>,
}

/// The settings of the session an actor is: a run, a build step or an edit
/// session (provisional). The default is a run's.
///
/// NOTICE: a session's ending (commit or discard) is the daemon's, not the
/// guest's: the guest streams a kept upper layer either way (see
/// `GuestSession` in `proto/needy/v1/guest.proto`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestSession {
  pub upper: UpperLayer,
  /// Off: init hands no exec to the proxy, whatever the route list says.
  pub routing: bool,
}

impl Default for GuestSession {
  fn default() -> Self {
    Self {
      upper: UpperLayer::Disposable,
      routing: true,
    }
  }
}

/// What happens to a session's upper layer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UpperLayer {
  /// Gone with the actor.
  #[default]
  Disposable,
  /// Archived and streamed with [`Guest::push_layer`] when the session
  /// ends.
  Kept,
}

/// The parts of an archive as the guest streams them; an error ends the
/// stream.
pub type Chunks = Pin<Box<dyn Stream<Item = Result<Vec<u8>>> + Send>>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuestUser {
  pub name: String,
  pub uid: u32,
  pub gid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestMount {
  pub target: String,
  pub executor: String,
  pub source: String,
  pub writable: bool,
  pub sftp_command: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestSensor {
  pub chip: String,
  pub idle_mc: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestCopy {
  pub name: String,
  pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestFault {
  /// The fault's name.
  pub name: String,
  pub at_ms: i64,
  pub temp_mc: i32,
}

/// What the daemon tells a running actor's init (`GuestService.Control`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestControl {
  /// The route list from now on, as [`GuestConfig::route_names`].
  Routes(Vec<String>),
  /// `run stop` asks the actor to end gracefully: the driving program gets
  /// SIGTERM, then SIGKILL after `grace`.
  Stop { grace: std::time::Duration },
}

/// The messages of [`Guest::control`], until the run ends.
pub type GuestControls = Pin<Box<dyn Stream<Item = GuestControl> + Send>>;

/// The actor's side of its run. Every call names the run the
/// actor's listener is bound to.
#[async_trait::async_trait]
pub trait Guest: Send + Sync + 'static {
  async fn config(&self, run: &str) -> Result<GuestConfig>;
  /// An event observed inside the actor, at `time_ms`.
  async fn event(&self, run: &str, time_ms: i64, event: Event) -> Result<()>;
  /// The driving program's exit.
  async fn exit(&self, run: &str, exit: Exit) -> Result<()>;
  /// A copy-in mapping's archive.
  async fn fetch_files(&self, run: &str, name: &str) -> Result<Vec<u8>>;
  /// A mirror-out mapping's archive.
  async fn push_files(&self, run: &str, name: &str, archive: Vec<u8>) -> Result<()>;
  /// The session's upper layer, a tar archive streamed as it is read, for
  /// a session whose upper layer is [`UpperLayer::Kept`].
  async fn push_layer(&self, run: &str, archive: Chunks) -> Result<()>;
  /// What init is told while the actor runs: the route list now and at
  /// each change, and a graceful stop when `run stop` asks for one. Init
  /// holds it for the actor's life; it ends when the run or the actor does.
  async fn control(&self, run: &str) -> Result<GuestControls>;
}

// -------------------------------------------------------------- runners

/// A join token to create.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewJoinToken {
  pub ttl_ms: i64,
  pub uses: u32,
  /// The roles a runner may declare; empty allows every role.
  pub roles: BTreeSet<RunnerRole>,
  /// Labels every runner that joins with it gets.
  pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinTokenState {
  Valid,
  Expired,
  UsedUp,
  Revoked,
}

/// A join token without its secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinTokenInfo {
  pub id: String,
  pub roles: BTreeSet<RunnerRole>,
  pub labels: BTreeMap<String, String>,
  pub created_ms: i64,
  pub expires_ms: i64,
  pub uses_left: u32,
  pub state: JoinTokenState,
}

/// A join token just created, with its secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedJoinToken {
  pub info: JoinTokenInfo,
  pub token: String,
}

/// What a runner declares when it joins and at each heartbeat.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunnerReport {
  /// What it can host; its roles must be within its join token's.
  pub capabilities: Capabilities,
  pub labels: BTreeMap<String, String>,
  /// Its machine; heartbeats carry it, a join does not.
  pub host: Option<RunnerHost>,
}

/// A runner's machine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunnerHost {
  /// As `std::env::consts` names them: `linux`, `macos`; `x86_64`, `aarch64`.
  pub os: String,
  pub arch: String,
  /// Its CPU and the CPUID entries its KVM supports, for recipe checks.
  pub cpu: needy_core::recipe::HostInfo,
  /// The home directory of the user the runner runs as: `~` in a native
  /// executor's paths there. Empty when unknown.
  pub home: String,
}

/// A runner asking to join.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Joining {
  pub join_token: String,
  pub name: String,
  pub report: RunnerReport,
}

/// A runner that joined, with its credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Joined {
  pub runner: RunnerInfo,
  pub credential: String,
  pub heartbeat_interval_ms: i64,
}

/// The answer to a heartbeat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Beat {
  /// A new credential after a rotation.
  pub credential: Option<String>,
  pub heartbeat_interval_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerStatus {
  Online,
  Offline,
  Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerInfo {
  pub name: String,
  /// As it last reported them.
  pub capabilities: Capabilities,
  pub labels: BTreeMap<String, String>,
  pub status: RunnerStatus,
  pub created_ms: i64,
  pub last_seen_ms: i64,
  pub rotating: bool,
  pub join_token: String,
  /// Its machine, once a heartbeat reported it to this control plane.
  pub host: Option<RunnerHost>,
}

/// What a bearer token is to the runner registry or the client registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credential {
  /// The credential of this runner.
  Runner(String),
  /// The token of this paired client.
  Client {
    id: String,
    name: String,
    access: ClientAccess,
  },
  /// Shaped like a runner credential or a client token, but nobody holds
  /// it: removed, revoked, rotated away, or never issued.
  Refused,
  /// Not a credential of this registry.
  Other,
}

/// What a runner sends on one of its streams; an error is the stream
/// failing, such as the runner's connection ending.
pub type FromRunner<T> = Pin<Box<dyn Stream<Item = std::result::Result<T, tonic::Status>> + Send>>;
/// What the control plane sends a runner on one of its streams.
pub type ToRunner<T> = Pin<Box<dyn Stream<Item = T> + Send>>;

/// The control plane's runner registry, and the streams runners open to it.
#[async_trait::async_trait]
pub trait Runners: Send + Sync + 'static {
  async fn create_join_token(&self, request: NewJoinToken) -> Result<CreatedJoinToken>;
  async fn join_tokens(&self) -> Result<Vec<JoinTokenInfo>>;
  async fn revoke_join_token(&self, id: &str) -> Result<()>;
  async fn join(&self, request: Joining) -> Result<Joined>;
  async fn heartbeat(&self, runner: &str, report: RunnerReport) -> Result<Beat>;
  /// Every runner, including removed ones.
  async fn runners(&self) -> Result<Vec<RunnerInfo>>;
  async fn rotate(&self, runner: &str) -> Result<()>;
  async fn remove(&self, runner: &str) -> Result<()>;
  /// Whose credential a bearer token is. The middleware calls it for every
  /// token that is not the control token. Backend failures are errors;
  /// invalid credentials are `Credential::Refused`.
  async fn credential(&self, token: &str) -> Result<Credential>;
  /// The control stream of `runner`: `results` are what it answers, the
  /// returned stream the tasks it gets. It replaces an earlier control
  /// stream of the same runner.
  // NOTICE: a runner's streams carry the exec protocol's own messages
  // (`ExecRequest`, `ExecResponse`) unchanged, so terminal behavior,
  // backpressure and refusals work as on any executor; converting them to
  // domain types here and back in the daemon would only copy them.
  async fn control(
    &self,
    runner: &str,
    results: FromRunner<needy_api_proto::v1::ControlRequest>,
  ) -> Result<ToRunner<needy_api_proto::v1::ControlResponse>>;
  /// The stream `runner` opened for its exec task `task`: `responses` are
  /// the exec's responses, the returned stream its requests.
  async fn task(
    &self,
    runner: &str,
    task: u64,
    responses: FromRunner<needy_api_proto::v1::TaskRequest>,
  ) -> Result<ToRunner<needy_api_proto::v1::TaskResponse>>;
}

// -------------------------------------------------------------- clients

/// The authority granted by the server, independent of transport.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ClientAccess {
  /// Legacy omitted grants can drive runs and processes.
  #[default]
  Runs,
  /// May also manage registered clients, runners and invitations.
  Admin,
}

/// A pairing code to create.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewPairingCode {
  pub access: ClientAccess,
  pub ttl_ms: i64,
  /// The client's name once it pairs; empty for a fresh one derived from
  /// the code's ID, which no client holds.
  pub name: String,
}

/// A pairing code just created, with its secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedPairingCode {
  pub access: ClientAccess,
  pub id: String,
  pub name: String,
  pub expires_ms: i64,
  pub code: String,
  /// The active client that holds `name` now: pairing with the code
  /// replaces it and revokes its token.
  pub replaces: Option<ClientInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInfo {
  pub access: ClientAccess,
  pub id: String,
  pub name: String,
  pub created_ms: i64,
  pub last_used_ms: Option<i64>,
  pub revoked_ms: Option<i64>,
}

/// A client that paired, with its token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paired {
  pub client: ClientInfo,
  pub token: String,
}

/// The control plane's client registry: pairing codes and the clients
/// that paired with them.
// TODO(clients): listing and revoking pairing codes is left out: a code is
// single-use and short-lived (10 minutes by default), so an unused one
// expires before it matters. Add `ListPairingCodes` and a revoke when an
// owner asks for longer-lived codes.
#[async_trait::async_trait]
pub trait Clients: Send + Sync + 'static {
  async fn create_pairing_code(&self, request: NewPairingCode) -> Result<CreatedPairingCode>;
  /// Spends a pairing code and returns the new client's token.
  async fn pair(&self, code: &str) -> Result<Paired>;
  /// Every client, including revoked ones.
  async fn clients(&self) -> Result<Vec<ClientInfo>>;
  async fn revoke(&self, id: &str) -> Result<()>;
  /// Whose token a bearer token is. The middleware calls it for every
  /// token that is not the control token. Backend failures are errors;
  /// invalid credentials are `Credential::Refused`.
  async fn credential(&self, token: &str) -> Result<Credential>;
}

// --------------------------------------------------------------- images

/// A request to build an image definition's layers and return its snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageBuild {
  /// Self-contained image definition YAML (its `extends` chain merged).
  pub image_yaml: String,
  /// Actor driver name; empty selects this host's VM driver.
  pub driver: String,
  /// Profile user for build plan cache keys; `None` uses the default user.
  pub user: Option<needy_config::profile::User>,
  /// Optional commit layer from the image's lockfile, stacked on top.
  pub commit: Option<needy_core::image::Commit>,
  /// Pinned feature digests (`canonical_ref -> sha256:<hex>`) from the
  /// image's lockfile.
  pub pinned_features: BTreeMap<String, String>,
}

/// One step of an image build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildStepOutcome {
  pub key: String,
  pub layer: String,
  pub cached: bool,
  pub ms: u64,
}

/// Result of [`Images::build`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageBuildResult {
  pub snapshot: String,
  pub commit_rebased: bool,
  pub steps: Vec<BuildStepOutcome>,
  /// Resolved feature digests (`canonical_ref -> sha256:<hex>`) for the CLI
  /// to record in the image's lockfile.
  pub resolved_features: BTreeMap<String, String>,
}

/// A request to start an interactive edit session for an image.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageEditStart {
  /// Stable identifier for the image (per-image edit lock and commit history key).
  pub image_name: String,
  /// Self-contained image definition YAML (its `extends` chain merged).
  pub image_yaml: String,
  /// Actor driver name; empty selects this host's VM driver.
  pub driver: String,
  /// Profile user for build plan cache keys; `None` uses the default user.
  pub user: Option<needy_config::profile::User>,
  /// Current commit from the image's lockfile, if any.
  pub commit: Option<needy_core::image::Commit>,
  /// Terminal size for the interactive shell.
  pub tty: Option<WindowSize>,
  /// Wait for the client to attach its terminal before starting the shell.
  pub wait_for_attach: bool,
  /// Pinned feature digests (`canonical_ref -> sha256:<hex>`) from the
  /// image's lockfile.
  pub pinned_features: BTreeMap<String, String>,
}

/// Result of [`Images::start_edit`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageEditStarted {
  pub session_id: String,
  pub commit_rebased: bool,
  /// Resolved feature digests (`canonical_ref -> sha256:<hex>`) for the CLI
  /// to record in the image's lockfile.
  pub resolved_features: BTreeMap<String, String>,
}

/// One program an edit session executed, as decided by the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedCommand {
  pub argv: Vec<String>,
  pub cwd: String,
}

/// Result of [`Images::finish_edit`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageEditFinished {
  /// The new commit, or `None` if the session exited non-zero, was aborted,
  /// or uploaded no upper layer.
  pub commit: Option<needy_core::image::Commit>,
  /// Programs executed during the edit session, in order.
  pub commands: Vec<RecordedCommand>,
  /// The shell's exit status, if reported.
  pub exit: Option<Exit>,
}

/// A request to step an image's commit pointer back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRollback {
  pub image_name: String,
  pub from_layer: String,
  pub steps: u32,
}

/// Result of [`Images::rollback`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRollbackResult {
  /// Target commit layer digest, or `None` when rolled back past the first commit.
  pub layer: Option<String>,
  /// Original base plan top digest the target commit was made on, or `None`
  /// when rolled back past the first commit or when the commit predates base
  /// persistence.
  pub base: Option<String>,
}

/// A request to inspect an image's commit history in the layer store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageLog {
  /// Stable identifier for the image (commit history key).
  pub image_name: String,
  /// Current commit layer digest from the lockfile, if any, used to mark the
  /// active commit.
  pub current_layer: Option<String>,
}

/// One recorded commit in an image's history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitHistoryEntry {
  pub layer: String,
  pub parent: Option<String>,
  pub base: Option<String>,
  pub commands: Vec<RecordedCommand>,
  pub time_ms: i64,
  pub active: bool,
}

/// Result of [`Images::log`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageLogResult {
  pub commits: Vec<CommitHistoryEntry>,
}

/// A request to build/resolve an image definition and stream it as an
/// OCI/Docker tar archive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageExport {
  /// Self-contained image definition YAML (its `extends` chain merged).
  pub image_yaml: String,
  /// Actor driver name; empty selects this host's VM driver.
  pub driver: String,
  /// Profile user for build plan cache keys; `None` uses the default user.
  pub user: Option<needy_config::profile::User>,
  /// Optional commit layer from the image's lockfile, stacked on top.
  pub commit: Option<needy_core::image::Commit>,
  /// Pinned feature digests (`canonical_ref -> sha256:<hex>`) from the
  /// image's lockfile.
  pub pinned_features: BTreeMap<String, String>,
  /// Optional `repository:tag` embedded in the exported archive's manifest.
  pub tag: Option<String>,
}

/// Request for [`Images::prune`]: garbage-collects unreferenced cached layers,
/// disks, OCI archives, stale cache keys, and `needy-snapshot:*` container
/// images on the control daemon.
#[derive(Debug, Clone, Default)]
pub struct ImagePrune {
  /// Also prune cached base OCI exports (`base-oci/*.tar`).
  pub all: bool,
  /// Compute what would be pruned without removing any files or images.
  pub dry_run: bool,
  /// Additional snapshot or layer digests (`sha256:<hex>`) to keep alongside
  /// live runs, active edit sessions, and recorded commit histories.
  pub keep: Vec<String>,
}

/// Summary returned by [`Images::prune`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImagePruneResult {
  pub layers_removed: u64,
  pub snapshots_removed: u64,
  pub bytes_reclaimed: u64,
  pub removed_images: Vec<String>,
}

/// Building image definitions, interactive edit sessions, commit history,
/// rollback, OCI archive export, and garbage collection.
#[async_trait::async_trait]
pub trait Images: Send + Sync + 'static {
  async fn build(&self, request: ImageBuild) -> Result<ImageBuildResult>;
  async fn start_edit(&self, request: ImageEditStart) -> Result<ImageEditStarted>;
  async fn finish_edit(&self, session_id: &str, abort: bool) -> Result<ImageEditFinished>;
  async fn rollback(&self, request: ImageRollback) -> Result<ImageRollbackResult>;
  async fn log(&self, request: ImageLog) -> Result<ImageLogResult>;
  async fn export(&self, request: ImageExport) -> Result<Chunks>;
  async fn prune(&self, request: ImagePrune) -> Result<ImagePruneResult>;
}
