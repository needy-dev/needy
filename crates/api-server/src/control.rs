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
use needy_core::event::{Event, ExecutorRef};
use needy_core::intercept::ExecContext;
use needy_core::types::{Origin, RunState, RunnerRole};
use tokio_stream::Stream;

/// Who is calling, fixed by the listener the call arrived on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
  /// A client of the control socket: the CLI, or another tool holding the
  /// daemon's token.
  Cli,
  /// An actor, on the listener bound to its run. Its calls act on that
  /// run; it may not create, stop or read the logs of runs.
  Actor { run: String },
  /// A runner, by the credential it presented on the control listener. It
  /// reaches `RunnerService.Heartbeat`, `Control` and `Task` only.
  Runner { name: String },
}

/// Why the daemon refused or failed a call. Each variant is one row of the
/// status table in `protocol::status`.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ControlError {
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
  /// Overrides the profile's driving program when not empty.
  pub actor_command: Vec<String>,
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
  pub created_ms: i64,
  pub profile: String,
  /// How the actor's driving program ended; `None` while it runs, without
  /// an actor, or when the VM ended before the program reported its exit.
  pub actor_exit: Option<Exit>,
  /// Why the run failed, with its causes.
  pub failure: Option<String>,
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
  /// A rule or a fault refused the command; the message is what the proxy
  /// prints.
  Refused(String),
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
  pub error: bool,
  pub message: String,
}

#[async_trait::async_trait]
pub trait Runs: Send + Sync + 'static {
  async fn create(&self, request: NewRun) -> Result<RunInfo>;
  /// A running, finished, stopped or failed run by ID.
  async fn get(&self, id: &str) -> Result<RunInfo>;
  async fn list(&self) -> Result<Vec<RunInfo>>;
  async fn stop(&self, id: &str) -> Result<()>;
  /// The actor's console output so far.
  async fn logs(&self, id: &str) -> Result<Vec<u8>>;
  /// Places a command the actor's proxy routed.
  async fn decide(&self, run: &str, ctx: ExecContext) -> Result<Route>;
  /// Checks a profile against the runners it would run on; creates nothing.
  async fn check(&self, profile: ProfileToCheck) -> Result<ProfileCheck>;
}

// ------------------------------------------------------------ processes

/// The first request of an exec stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecOpen {
  Start(ExecSpec),
  /// `window` is the attaching client's terminal size, if it has one.
  Attach {
    process: String,
    replay: bool,
    window: Option<WindowSize>,
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
  /// The daemon ends the stream before the process exited, for this
  /// reason; nothing follows.
  Ended(String),
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
  async fn kill(&self, process: &str, signal: &str) -> Result<()>;
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
  pub hwmon_faults: Vec<GuestFault>,
}

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
  pub at_ms: i64,
  pub temp_mc: i32,
}

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
  pub roles: BTreeSet<RunnerRole>,
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
  pub roles: BTreeSet<RunnerRole>,
  pub labels: BTreeMap<String, String>,
  pub status: RunnerStatus,
  pub created_ms: i64,
  pub last_seen_ms: i64,
  pub rotating: bool,
  pub join_token: String,
}

/// What a bearer token is to the runner registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credential {
  /// The credential of this runner.
  Runner(String),
  /// Shaped like a runner credential, but no runner holds it: removed,
  /// rotated away, or never issued.
  Refused,
  /// Not a runner credential.
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
  /// token that is not the control token.
  async fn credential(&self, token: &str) -> Credential;
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
