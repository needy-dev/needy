//! The lifecycles `state.db` keeps besides a run's (`crate::run::state`):
//! a run's hold on an instance, a process, and a runner. Each is an enum
//! with an `allowed(from, to)` table; the store writes a state only through
//! it (`Store::start_instance`, `Store::advance_instance`,
//! `Store::start_process`, `Store::advance_process`, `Store::remove`) and
//! refuses any other change with [`Refused`].
//!
//! A run's hold on an actor or executor instance:
//!
//! ```text
//! (new) ──► Running ──► Released (the run gave it back)
//!              ├──────► Ended    (reconciliation ended it)
//!              └──────► Lost     (nobody reported it after a restart)
//! ```
//!
//! A process one of a run's executors ran:
//!
//! ```text
//! (new) ──► Running ──► Exited (it exited)
//!              ├──────► Ended  (killed because its run ended)
//!              └──────► Lost   (nobody reported it after a restart)
//! ```
//!
//! A runner, as stored:
//!
//! ```text
//! (join) ──► Joined ──► Removed
//! ```
//!
//! `runner ls` shows a joined runner as online or offline by its last
//! heartbeat (`crate::runners`): derived, not stored, like a join token's
//! state. A removed runner's name may join again, which makes a new runner.
//!
//! Every end state is terminal: the first one reached stays, as a run's
//! does. Two enders racing (a relay that saw a process exit and a run's end
//! that killed it, a runner's late report and the wait that marked its
//! instance lost) leave the first one's state.

use serde::{Deserialize, Serialize};

/// Where a run's hold on an instance stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceState {
  /// The run holds it.
  Running,
  /// The run gave it back when it ended.
  Released,
  /// Ended by reconciliation: a restarted control plane, or a runner that
  /// reported it for a run that is no longer live.
  Ended,
  /// The control plane restarted and nobody reported it: whether it still
  /// runs is unknown.
  // NOTICE: a runner that reports a lost instance's run later is told to
  // end it (`reconcile_runner`), but the row stays lost: lost is terminal.
  Lost,
}

impl InstanceState {
  /// The state a new hold starts in.
  pub const START: Self = Self::Running;

  pub fn as_str(self) -> &'static str {
    match self {
      Self::Running => "running",
      Self::Released => "released",
      Self::Ended => "ended",
      Self::Lost => "lost",
    }
  }

  pub fn allowed(from: Self, to: Self) -> bool {
    use InstanceState::*;
    matches!((from, to), (Running, Released | Ended | Lost))
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
  Running,
  /// It exited; its exit is unknown when nobody saw it.
  Exited,
  /// Killed because its run ended.
  Ended,
  /// The control plane restarted and nobody reported it.
  Lost,
}

impl ProcessState {
  /// The state a new process starts in.
  pub const START: Self = Self::Running;

  pub fn as_str(self) -> &'static str {
    match self {
      Self::Running => "running",
      Self::Exited => "exited",
      Self::Ended => "ended",
      Self::Lost => "lost",
    }
  }

  pub fn allowed(from: Self, to: Self) -> bool {
    use ProcessState::*;
    matches!((from, to), (Running, Exited | Ended | Lost))
  }
}

/// A runner's stored state. Online and offline are what `runner ls` shows
/// of a joined runner, by its last heartbeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerState {
  /// It joined and holds a credential.
  Joined,
  /// Its credentials were revoked; it stays listed.
  Removed,
}

impl RunnerState {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Joined => "joined",
      Self::Removed => "removed",
    }
  }

  pub fn allowed(from: Self, to: Self) -> bool {
    use RunnerState::*;
    matches!((from, to), (Joined, Removed))
  }
}

/// A change of state its lifecycle does not allow, or a start of a row that
/// exists.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{object} {id} is {from}; it cannot become {to}")]
pub struct Refused {
  /// `instance`, `process` or `runner`.
  pub object: &'static str,
  pub id: String,
  pub from: &'static str,
  pub to: &'static str,
}

#[cfg(test)]
#[path = "lifecycle_test.rs"]
mod tests;
