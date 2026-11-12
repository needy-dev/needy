//! An actor's state: the one place where a run's `core::ActorState`
//! changes. The daemon advances it the same way for every actor driver, from
//! what it sees itself, never from what a driver says:
//!
//! ```text
//! (none) ──► Starting ──► Configured ──► Ready ──► Exited
//!               │             │                     ▲
//!               └─────────────┴─────────────────────┘
//! ```
//!
//! - `Starting`: `actor::start` asks the driver to start it (`actor.start`
//!   in the trace once the driver did).
//! - `Configured`: its guest fetched the run's configuration
//!   (`actor.config`).
//! - `Ready`: its process service answered a request (`actor.ready`). A
//!   socket that accepts is not enough: vfkit's and Apple `container`'s
//!   accept on the host before the guest listens. Only an actor whose
//!   driving program has a terminal serves a process service, so one
//!   without a terminal goes from `Configured` to `Exited`.
//! - `Exited`: its VM or container exited, or its run stopped it
//!   (`actor.vm.exit` when it exited by itself). Terminal.
//!
//! A run has no actor state until its actor starts: while its executors
//! are acquired, or when its create failed before.

use std::sync::Mutex;

use needy_core::types::ActorState;

/// A run's actor's current state; `None` before it starts.
#[derive(Debug, Default)]
pub struct State(Mutex<Option<ActorState>>);

/// A transition the current state does not allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refused {
  pub from: Option<ActorState>,
  pub to: ActorState,
}

impl std::fmt::Display for Refused {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self.from {
      Some(from) => write!(f, "a {from} actor cannot become {}", self.to),
      None => write!(f, "an actor that did not start cannot become {}", self.to),
    }
  }
}

impl State {
  pub fn get(&self) -> Option<ActorState> {
    *self.lock()
  }

  /// Moves to `to` and returns the state before, or refuses.
  pub fn advance(&self, to: ActorState) -> Result<Option<ActorState>, Refused> {
    let mut state = self.lock();
    let from = *state;
    if !allowed(from, to) {
      return Err(Refused { from, to });
    }
    *state = Some(to);
    Ok(from)
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, Option<ActorState>> {
    // A panic elsewhere cannot leave the state half-written; it is one value.
    self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }
}

fn allowed(from: Option<ActorState>, to: ActorState) -> bool {
  use ActorState::*;
  matches!((from, to), (None, Starting) | (Some(Starting), Configured | Exited) | (Some(Configured), Ready | Exited) | (Some(Ready), Exited))
}

#[cfg(test)]
#[path = "state_test.rs"]
mod tests;
