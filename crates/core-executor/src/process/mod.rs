//! The process host: runs processes on this machine and keeps them
//! independent of clients.
//!
//! A process belongs to the [`Host`], not to the stream that started it.
//! Output is fanned out to attached clients and kept in a bounded replay
//! buffer ([`Proc`]), so a client may disconnect and attach again. A finished
//! process stays listed for the host's retention period, then expires.

mod pipes;
mod proc;
mod pty;

#[cfg(unix)]
#[path = "platform/unix.rs"]
mod platform;
#[cfg(windows)]
#[path = "platform/windows.rs"]
mod platform;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
pub use needy_common::process::Exit;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

pub use proc::Proc;

/// How long a finished process stays in [`Host::list`] and can be attached.
// NOTICE: long enough to `needy ps` and `needy attach` a command that just
// finished, short enough that the table of a long-running executor stays
// small. Each finished process keeps at most its replay buffer (256 KiB).
pub const RETENTION: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("empty command")]
  EmptyCommand,
  /// The cause is part of the message, which becomes the status a client
  /// prints, so it is not also a `source`.
  #[error("cannot start {program}: {error}")]
  Start {
    program: String,
    error: std::io::Error,
  },
  /// portable-pty reports its errors as `anyhow` values; they are kept as text.
  #[error("cannot start {program}: {message}")]
  StartTerminal { program: String, message: String },
  #[error("cannot open a terminal: {0}")]
  Terminal(String),
  #[error("the host is shutting down")]
  ShuttingDown,
}

#[derive(Debug, Clone)]
pub enum Output {
  Stdout(Bytes),
  Stderr(Bytes),
  Exit(Exit),
}

#[derive(Debug)]
pub enum Input {
  Stdin(Vec<u8>),
  Eof,
  /// Signal name, for example `SIGINT`.
  Signal(String),
  Resize(u16, u16),
}

#[derive(Debug, Clone, Default)]
pub struct Spec {
  pub argv: Vec<String>,
  pub cwd: String,
  pub env: Vec<(String, String)>,
  pub clear_env: bool,
  /// Rows and columns of a terminal; pipes when `None`.
  pub tty: Option<(u16, u16)>,
  pub no_stdin: bool,
  pub session_id: String,
}

/// The process table of one machine. Clones share the table.
#[derive(Clone)]
pub struct Host {
  inner: Arc<Inner>,
}

struct Inner {
  procs: Mutex<HashMap<String, Arc<Proc>>>,
  retention: Duration,
  /// Every task the host's processes and streams run on.
  tasks: TaskTracker,
  cancel: CancellationToken,
}

impl Default for Host {
  fn default() -> Self {
    Self::new(RETENTION)
  }
}

impl Host {
  /// A host whose finished processes expire after `retention`.
  pub fn new(retention: Duration) -> Self {
    Self {
      inner: Arc::new(Inner {
        procs: Mutex::new(HashMap::new()),
        retention,
        tasks: TaskTracker::new(),
        cancel: CancellationToken::new(),
      }),
    }
  }

  pub fn get(&self, id: &str) -> Option<Arc<Proc>> {
    self.table().get(id).cloned()
  }

  /// Running processes and those that finished within the retention period,
  /// oldest first.
  pub fn list(&self) -> Vec<Arc<Proc>> {
    let mut all: Vec<_> = self.table().values().cloned().collect();
    all.sort_by_key(|p| p.started_at_ms);
    all
  }

  /// Starts a process. Its tasks belong to the host, not to the caller.
  pub fn spawn(&self, spec: Spec) -> Result<Arc<Proc>, Error> {
    if spec.argv.first().is_none_or(|program| program.is_empty()) {
      return Err(Error::EmptyCommand);
    }
    if self.inner.cancel.is_cancelled() {
      return Err(Error::ShuttingDown);
    }
    let proc = match spec.tty {
      Some(size) => pty::spawn(&spec, size, &self.inner.tasks, &self.inner.cancel)?,
      None => pipes::spawn(&spec, &self.inner.tasks, &self.inner.cancel)?,
    };
    self.table().insert(proc.id.clone(), proc.clone());
    Ok(proc)
  }

  /// Kills every running process and waits for the host's tasks to end.
  /// The executor role calls it on a termination signal, and
  /// `Daemon::shutdown` for the control daemon's local executor.
  pub async fn shutdown(&self) {
    self.inner.cancel.cancel();
    for proc in self.list() {
      if proc.exit().is_none() {
        proc.send(Input::Signal("SIGKILL".into()));
      }
    }
    self.inner.tasks.close();
    self.inner.tasks.wait().await;
  }

  /// The tracker and token stream tasks of this host's processes run under.
  pub(crate) fn tasks(&self) -> (&TaskTracker, &CancellationToken) {
    (&self.inner.tasks, &self.inner.cancel)
  }

  /// The table after dropping processes whose retention ran out. Expiry is
  /// checked on access, so no timer task is needed per process.
  fn table(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Proc>>> {
    let mut procs = self.inner.procs.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let retention = self.inner.retention;
    procs.retain(|_, proc| !proc.finished_for().is_some_and(|age| age >= retention));
    procs
  }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
