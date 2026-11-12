//! A graceful stop of a run's actor, which its guest waits for on its
//! control stream (`Guest::control`, `GuestControl::Stop`): `run stop` asks
//! it, with the driving program's grace period, only while a guest waits, so
//! a guest that never asks (one that has not finished starting, or a guest
//! needy older than graceful stops) is killed at once instead of waited for.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::watch;

#[derive(Debug)]
pub struct StopRequest {
  /// The grace period, once a stop asked for one.
  grace: watch::Sender<Option<Duration>>,
  /// How many guest calls wait for it now.
  waiting: AtomicUsize,
}

impl Default for StopRequest {
  fn default() -> Self {
    Self {
      grace: watch::channel(None).0,
      waiting: AtomicUsize::new(0),
    }
  }
}

/// Counts a waiting call until it returns or is dropped, as when the
/// guest's connection closes.
struct Waiting<'a>(&'a AtomicUsize);

impl Drop for Waiting<'_> {
  fn drop(&mut self) {
    self.0.fetch_sub(1, Ordering::SeqCst);
  }
}

impl StopRequest {
  /// Returns the grace period once a stop asks for it.
  pub async fn wait(&self) -> Duration {
    self.waiting.fetch_add(1, Ordering::SeqCst);
    let _waiting = Waiting(&self.waiting);
    let mut grace = self.grace.subscribe();
    match grace.wait_for(Option::is_some).await {
      Ok(grace) => grace.unwrap_or_default(),
      // The sender lives as long as the run, which outlives its guest
      // calls; a dropped one stops at once.
      Err(_) => Duration::ZERO,
    }
  }

  /// Asks the waiting guest to stop its driving program with `grace`, and
  /// says whether a guest was waiting to be asked.
  pub fn ask(&self, grace: Duration) -> bool {
    if self.waiting.load(Ordering::SeqCst) == 0 {
      return false;
    }
    self.grace.send_replace(Some(grace));
    true
  }

  /// Whether a guest waits to be asked.
  #[cfg(test)]
  pub fn waited_for(&self) -> bool {
    self.waiting.load(Ordering::SeqCst) > 0
  }
}

#[cfg(test)]
#[path = "stop_test.rs"]
mod tests;
