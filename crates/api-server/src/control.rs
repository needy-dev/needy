//! What the daemon implements: the domain traits behind the gRPC services,
//! who is calling ([`Caller`]), and the errors ([`ControlError`]) that
//! `protocol::status` maps to gRPC statuses in one table.
//!
//! Nothing here is generated code. The adapters in `protocol::grpc` convert
//! requests into these types, call the traits, and convert the results back.

use std::collections::HashMap;
use std::pin::Pin;

use needy_common::process::Exit;
use needy_core::event::{Event, ExecutorRef};
use needy_core::intercept::ExecContext;
use needy_core::types::{Origin, SessionState};
use tokio_stream::Stream;

/// Who is calling, fixed by the listener the call arrived on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
  /// A client of the control socket: the CLI, or another tool holding the
  /// daemon's token.
  Cli,
  /// An actor, on the listener bound to its session. Its calls act on that
  /// session; it may not create, stop or read the logs of sessions.
  Actor { session: String },
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
  #[error("{0}")]
  NotFound(String),
  /// The request is malformed.
  #[error("{0}")]
  Invalid(String),
  /// The request cannot be carried out, such as a session whose profile is
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

// ------------------------------------------------------------- sessions

/// A session to create from a profile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewSession {
  /// The profile, self-contained: no `extends` to read, host paths
  /// absolute.
  pub profile_yaml: String,
  /// The actor's recipe when the profile names a recipe file; empty for a
  /// built-in recipe or none.
  pub recipe_yaml: String,
  /// The profile's name, derived by the client from its file name.
  pub profile_name: String,
  /// Empty for the session's ID.
  pub group: String,
  /// Empty for the session's ID.
  pub name: String,
  pub start_actor: bool,
  /// Overrides the profile's driving program when not empty.
  pub actor_command: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
  pub id: String,
  pub name: String,
  pub group: String,
  pub state: SessionState,
  /// The executors the session holds.
  pub executors: Vec<ExecutorRef>,
  /// The executors the session held and gave back when it stopped,
  /// finished or failed.
  pub released: Vec<ExecutorRef>,
  pub created_ms: i64,
  pub profile: String,
  /// How the actor's driving program ended; `None` while it runs, without
  /// an actor, or when the VM ended before the program reported its exit.
  pub actor_exit: Option<Exit>,
  /// Why the session failed, with its causes.
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

#[async_trait::async_trait]
pub trait Sessions: Send + Sync + 'static {
  async fn create(&self, request: NewSession) -> Result<SessionInfo>;
  /// A running, finished, stopped or failed session by ID.
  async fn get(&self, id: &str) -> Result<SessionInfo>;
  async fn list(&self) -> Result<Vec<SessionInfo>>;
  async fn stop(&self, id: &str) -> Result<()>;
  /// The actor's console output so far.
  async fn logs(&self, id: &str) -> Result<Vec<u8>>;
  /// Places a command the actor's proxy routed.
  async fn decide(&self, session: &str, ctx: ExecContext) -> Result<Route>;
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
  /// Empty outside a session.
  pub session: String,
  /// Empty to let the session's rules decide.
  pub executor: String,
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
  pub session: String,
  /// Who started the process; `None` for a process the daemon did not start.
  pub origin: Option<Origin>,
}

#[async_trait::async_trait]
pub trait Processes: Send + Sync + 'static {
  /// Starts or attaches to a process; `input` is the rest of the caller's
  /// stream.
  async fn exec(&self, open: ExecOpen, input: Inputs) -> Result<Outputs>;
  /// The processes of `session`, or of every executor when it is empty.
  async fn list(&self, session: &str) -> Result<Vec<ProcessInfo>>;
  async fn kill(&self, process: &str, signal: &str) -> Result<()>;
}

// ---------------------------------------------------------------- guest

/// The actor's configuration, which its init asks for at boot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuestConfig {
  pub session: String,
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

/// The actor's side of its session. Every call names the session the
/// actor's listener is bound to.
#[async_trait::async_trait]
pub trait Guest: Send + Sync + 'static {
  async fn config(&self, session: &str) -> Result<GuestConfig>;
  /// An event observed inside the actor, at `time_ms`.
  async fn event(&self, session: &str, time_ms: i64, event: Event) -> Result<()>;
  /// The driving program's exit.
  async fn exit(&self, session: &str, exit: Exit) -> Result<()>;
  /// A copy-in mapping's archive.
  async fn fetch_files(&self, session: &str, name: &str) -> Result<Vec<u8>>;
  /// A mirror-out mapping's archive.
  async fn push_files(&self, session: &str, name: &str, archive: Vec<u8>) -> Result<()>;
}
