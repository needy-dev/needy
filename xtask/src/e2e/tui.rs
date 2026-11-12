//! The hook for TUI fidelity cases: full-screen programs whose screen in
//! an attached terminal must match what they drew.
//!
//! Each case becomes a matrix cell per actor driver, `DRIVER/tui/NAME`, run
//! with `run create -it` on a pseudo-terminal: the case names the driving
//! program and drives it through [`super::term::Term`], which keeps the
//! screen as `vt100` parses it.
//!
//! TODO(tui-fidelity): no case is registered yet. The TUI fidelity harness
//! (`cargo xtask tui`, `xtask/src/tui/`, scenarios in `xtask/tui/`) landed
//! alongside; its scenarios drive real agents that need accounts, so none
//! is a cell yet. A case that needs no account (a small curses program in
//! the actor image) joins here by adding it to [`cases`]: its
//! [`Case::drive`] can run a `.scenario` through `cargo xtask tui` against
//! `needy run create -it`, or drive [`Term`] directly. Until then the
//! dimension is empty, not skipped.

use anyhow::Result;

use super::term::Term;

/// One TUI fidelity case.
pub trait Case: Send + Sync {
  /// The name in the cell ID: `DRIVER/tui/NAME`.
  fn name(&self) -> &'static str;
  /// The driving program (`actor.command`), which must be in the actor
  /// image.
  fn command(&self) -> Vec<String>;
  /// Drives the attached program and checks its screen. An error fails
  /// the cell with its message.
  fn drive(&self, term: &mut Term) -> Result<()>;
}

/// Every registered case.
pub fn cases() -> Vec<Box<dyn Case>> {
  Vec::new()
}
