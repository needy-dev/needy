//! Temporary terminal feedback while a command waits for an operation.

use std::time::Duration;

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

/// A scoped spinner on stderr. Finish it before printing an outcome or
/// handing the terminal to an interactive session; errors clear it on drop.
///
/// Redirected stderr, dumb terminals, and nonempty `NO_COLOR` remain silent.
#[must_use = "keep the progress guard alive until the operation finishes"]
pub struct Progress {
  bar: ProgressBar,
}

impl Progress {
  /// Starts progress when enabled and stderr supports terminal rendering.
  /// Callers can disable it for machine-readable diagnostic modes.
  pub fn new(enabled: bool, message: impl Into<String>) -> Self {
    let terminal_feedback = enabled && !crate::logging::stderr_active() && std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty());
    let target = if terminal_feedback {
      ProgressDrawTarget::stderr()
    } else {
      ProgressDrawTarget::hidden()
    };
    Self::with_draw_target(target, message)
  }

  fn with_draw_target(target: ProgressDrawTarget, message: impl Into<String>) -> Self {
    // TODO: Delay the first draw when indicatif supports it without another
    // worker; this slice keeps all animation lifetime owned by its ticker.
    let bar = ProgressBar::with_draw_target(None, target).with_style(ProgressStyle::default_spinner()).with_message(message.into());
    if !bar.is_hidden() {
      bar.tick();
      // NOTICE: indicatif owns and joins its ticker; no task outlives this guard.
      // Source: https://github.com/console-rs/indicatif/blob/0.18.6/src/progress_bar.rs#L692-L700
      // Reference: https://docs.rs/indicatif/0.18.6/indicatif/struct.ProgressBar.html#method.enable_steady_tick
      bar.enable_steady_tick(Duration::from_millis(80));
    }
    Self { bar }
  }

  /// Replaces the description while the operation continues.
  pub fn message(&self, message: impl Into<String>) {
    self.bar.set_message(message.into());
  }

  /// Whether terminal feedback replaces the caller's plain progress line.
  pub fn is_visible(&self) -> bool {
    !self.bar.is_hidden()
  }

  /// Stops animation and clears the spinner before subsequent output.
  pub fn finish(self) {
    drop(self);
  }
}

impl Drop for Progress {
  fn drop(&mut self) {
    // Join before clearing so no later tick can overwrite the command's output.
    self.bar.disable_steady_tick();
    self.bar.finish_and_clear();
  }
}

#[cfg(test)]
mod tests {
  use std::time::Instant;

  use indicatif::InMemoryTerm;

  use super::*;

  fn progress(term: &InMemoryTerm) -> Progress {
    Progress::with_draw_target(ProgressDrawTarget::term_like(Box::new(term.clone())), "Connecting")
  }

  #[test]
  fn renders_updates_and_clears_before_the_next_output() {
    let term = InMemoryTerm::new(4, 80);
    let progress = progress(&term);
    assert!(term.contents().contains("Connecting"));

    progress.message("Creating run");
    assert!(term.contents().contains("Creating run"));
    assert!(!term.contents().contains("Connecting"));

    progress.finish();
    assert_eq!(term.contents(), "");
  }

  #[test]
  fn error_return_clears_the_spinner() {
    fn operation(term: &InMemoryTerm) -> Result<(), &'static str> {
      let _progress = progress(term);
      assert!(term.contents().contains("Connecting"));
      Err("operation failed")
    }

    let term = InMemoryTerm::new(4, 80);
    assert_eq!(operation(&term), Err("operation failed"));
    assert_eq!(term.contents(), "");
  }

  #[test]
  fn a_waiting_operation_animates_and_stays_cleared_after_drop() {
    let term = InMemoryTerm::new(4, 80);
    let progress = progress(&term);
    let first_frame = term.contents();
    let deadline = Instant::now() + Duration::from_secs(2);
    while term.contents() == first_frame && Instant::now() < deadline {
      std::thread::sleep(Duration::from_millis(10));
    }
    assert_ne!(term.contents(), first_frame, "spinner did not advance while waiting");

    drop(progress);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(term.contents(), "");
  }
}
