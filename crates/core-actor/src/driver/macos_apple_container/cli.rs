//! Apple's `container` CLI, the boundary the `macos-apple-container` driver
//! drives: the trait tests stand in for, and the CLI as a child process.

use std::process::ExitStatus;

use crate::driver::Error;

/// The CLI's program name, found on `PATH`.
pub const PROGRAM: &str = "container";

/// What a finished CLI command said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
  pub success: bool,
  pub stdout: String,
  pub stderr: String,
}

/// Apple's `container` CLI, as far as the driver uses it. A real boundary
/// (a container runtime), so tests stand in for it.
#[async_trait::async_trait]
pub trait Cli: Send + Sync {
  /// Runs `container ARGS` to its end.
  async fn output(&self, args: &[String]) -> Result<Output, Error>;
  /// Starts `container ARGS` without input, its stdout and stderr written to
  /// `log`.
  fn spawn(&self, args: &[String], log: std::fs::File) -> Result<Box<dyn Process>, Error>;
}

/// A started CLI process.
#[async_trait::async_trait]
pub trait Process: Send {
  fn id(&self) -> u32;
  async fn wait(&mut self) -> std::io::Result<ExitStatus>;
  /// Kills the CLI process itself (not the container).
  fn kill(&mut self) -> std::io::Result<()>;
}

/// The CLI as a child process of the daemon.
pub(super) struct Command;

impl Command {
  fn command(args: &[String]) -> Result<tokio::process::Command, Error> {
    if !cfg!(target_os = "macos") {
      return Err(Error::Unsupported("Apple's `container` runs on macOS only, on a Mac with Apple silicon".into()));
    }
    let mut command = tokio::process::Command::new(PROGRAM);
    command.args(args).stdin(std::process::Stdio::null()).kill_on_drop(false);
    Ok(command)
  }
}

/// Why the CLI could not be started.
fn not_started(e: std::io::Error) -> Error {
  if e.kind() == std::io::ErrorKind::NotFound {
    Error::AppleContainer(
      "Apple's `container` CLI is not on PATH; install it from https://github.com/apple/container/releases and run `container system start`"
        .into(),
    )
  } else {
    Error::AppleContainer(format!("cannot run `container`: {e}"))
  }
}

#[async_trait::async_trait]
impl Cli for Command {
  async fn output(&self, args: &[String]) -> Result<Output, Error> {
    let output = Self::command(args)?.output().await.map_err(not_started)?;
    Ok(Output {
      success: output.status.success(),
      stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
      stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
  }

  fn spawn(&self, args: &[String], log: std::fs::File) -> Result<Box<dyn Process>, Error> {
    let stderr = log.try_clone().map_err(|e| Error::AppleContainer(format!("cannot open the actor's log: {e}")))?;
    let child = Self::command(args)?.stdout(log).stderr(stderr).spawn().map_err(not_started)?;
    Ok(Box::new(Child(child)))
  }
}

struct Child(tokio::process::Child);

#[async_trait::async_trait]
impl Process for Child {
  fn id(&self) -> u32 {
    self.0.id().unwrap_or(0)
  }

  async fn wait(&mut self) -> std::io::Result<ExitStatus> {
    self.0.wait().await
  }

  fn kill(&mut self) -> std::io::Result<()> {
    self.0.start_kill()
  }
}
