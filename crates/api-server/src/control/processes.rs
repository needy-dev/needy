//! Domain types and trait for `ProcessService`.

use std::collections::HashMap;
use std::pin::Pin;

use needy_common::process::Exit;
use needy_core::event::Event;
use needy_core::types::{Origin, Principal};
use tokio_stream::Stream;

use super::{Diagnostic, Result, WindowSize};

/// A stream the daemon reads from a caller, or writes to one.
pub type Inputs = Pin<Box<dyn Stream<Item = ExecInput> + Send>>;
pub type Outputs = Pin<Box<dyn Stream<Item = Result<ExecOutput>> + Send>>;

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
