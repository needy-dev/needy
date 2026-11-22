//! A run's state: the one place where its `core::RunState` changes.
//!
//! ```text
//! Creating ──► Running ──► Stopping ──► Stopped
//!    │            │
//!    ├──► Failed  ├──► Finished (the actor's driving program exited)
//!    └──► Finished└──► Failed (the actor exited before it did)
//! ```
//!
//! `Stopped`, `Finished` and `Failed` are terminal: the first one reached
//! stays. A stop that has begun ends as `Stopped`, even if the actor exits
//! meanwhile.

use std::sync::Mutex;

use needy_core::types::RunState;

/// A run's current state.
#[derive(Debug)]
pub struct State(Mutex<RunState>);

/// A transition the current state does not allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refused {
  pub from: RunState,
  pub to: RunState,
}

impl std::fmt::Display for Refused {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "a {} run cannot become {}", self.from.as_str(), self.to.as_str())
  }
}

impl State {
  /// A run being created.
  pub fn new() -> Self {
    Self(Mutex::new(RunState::Creating))
  }

  /// Reconstitutes the persisted state of an invocation whose holder was verified.
  pub fn restore(state: RunState) -> Self {
    Self(Mutex::new(state))
  }

  pub fn get(&self) -> RunState {
    *self.lock()
  }

  /// Moves to `to` and returns the state before, or refuses.
  pub fn advance(&self, to: RunState) -> Result<RunState, Refused> {
    let mut state = self.lock();
    let from = *state;
    if !allowed(from, to) {
      return Err(Refused { from, to });
    }
    *state = to;
    Ok(from)
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, RunState> {
    needy_common::sync::lock(&self.0)
  }
}

fn allowed(from: RunState, to: RunState) -> bool {
  use RunState::*;
  matches!((from, to), (Creating, Running | Failed | Finished) | (Running, Stopping | Finished | Failed) | (Stopping, Stopped))
}

#[cfg(test)]
#[path = "state_test.rs"]
mod tests;
