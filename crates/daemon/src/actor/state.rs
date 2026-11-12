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

use needy_core::types::ActorState;
use tokio::sync::watch;

/// A run's actor's current state; `None` before it starts. A watch, so a
/// graceful stop can wait for the actor to exit.
#[derive(Debug)]
pub struct State(watch::Sender<Option<ActorState>>);

impl Default for State {
  fn default() -> Self {
    Self(watch::channel(None).0)
  }
}

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
    *self.0.borrow()
  }

  /// Moves to `to` and returns the state before, or refuses.
  pub fn advance(&self, to: ActorState) -> Result<Option<ActorState>, Refused> {
    let mut result = Ok(None);
    self.0.send_if_modified(|state| {
      let from = *state;
      if !allowed(from, to) {
        result = Err(Refused { from, to });
        return false;
      }
      *state = Some(to);
      result = Ok(from);
      true
    });
    result
  }

  /// Returns once the actor exited.
  pub async fn exited(&self) {
    let mut state = self.0.subscribe();
    // The sender lives as long as `self`, so the wait ends only on `Exited`.
    if state.wait_for(|s| *s == Some(ActorState::Exited)).await.is_err() {
      tracing::debug!("the actor's state went away before it exited");
    }
  }
}

fn allowed(from: Option<ActorState>, to: ActorState) -> bool {
  use ActorState::*;
  matches!((from, to), (None, Starting) | (Some(Starting), Configured | Exited) | (Some(Configured), Ready | Exited) | (Some(Ready), Exited))
}

#[cfg(test)]
#[path = "state_test.rs"]
mod tests;
