//! A session's state: the one place where its `core::SessionState` changes.
//!
//! ```text
//! Creating ──► Running ──► Stopping ──► Stopped
//!    │            │
//!    ├──► Failed  └──► Finished (the actor's driving program exited)
//!    └──► Finished
//! ```
//!
//! `Stopped`, `Finished` and `Failed` are terminal: the first one reached
//! stays. A stop that has begun ends as `Stopped`, even if the actor exits
//! meanwhile.

use std::sync::Mutex;

use needy_core::types::SessionState;

/// A session's current state.
#[derive(Debug)]
pub struct State(Mutex<SessionState>);

/// A transition the current state does not allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refused {
  pub from: SessionState,
  pub to: SessionState,
}

impl std::fmt::Display for Refused {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "a {} session cannot become {}", self.from.as_str(), self.to.as_str())
  }
}

impl State {
  /// A session being created.
  pub fn new() -> Self {
    Self(Mutex::new(SessionState::Creating))
  }

  pub fn get(&self) -> SessionState {
    *self.lock()
  }

  /// Moves to `to` and returns the state before, or refuses.
  pub fn advance(&self, to: SessionState) -> Result<SessionState, Refused> {
    let mut state = self.lock();
    let from = *state;
    if !allowed(from, to) {
      return Err(Refused { from, to });
    }
    *state = to;
    Ok(from)
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, SessionState> {
    // A panic elsewhere cannot leave the state half-written; it is one value.
    self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }
}

fn allowed(from: SessionState, to: SessionState) -> bool {
  use SessionState::*;
  matches!((from, to), (Creating, Running | Failed | Finished) | (Running, Stopping | Finished) | (Stopping, Stopped))
}

#[cfg(test)]
#[path = "state_test.rs"]
mod tests;
