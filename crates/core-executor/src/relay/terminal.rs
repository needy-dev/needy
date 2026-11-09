//! The local terminal: whether there is one, its window size, and raw mode
//! for the length of a relay (`crossterm`).

use std::io::IsTerminal;

use needy_api_proto::v1::WindowSize;

/// Whether stdin and stdout are both terminals, so a command gets a PTY by
/// default.
pub fn interactive() -> bool {
  // NOTICE: a Windows client never picks a terminal by itself. Terminals on
  // a Windows executor do not work yet (see the closed-loop results), and a
  // console in raw mode has not been tried as a client; `-t` still forces one.
  cfg!(unix) && std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// The local window size, or 24x80 when there is no terminal.
pub fn window() -> WindowSize {
  match crossterm::terminal::size() {
    Ok((cols, rows)) => WindowSize {
      rows: u32::from(rows.max(1)),
      cols: u32::from(cols.max(1)),
    },
    Err(e) => {
      tracing::debug!("relay: no window size: {e}");
      WindowSize { rows: 24, cols: 80 }
    }
  }
}

/// The local terminal in raw mode; dropping it restores the terminal.
pub struct RawMode(());

impl RawMode {
  /// Raw mode, or `None` when stdin is not a terminal to put in it.
  pub fn enable() -> Option<Self> {
    // crossterm would fall back to the controlling terminal; a relay whose
    // stdin is a pipe leaves that terminal alone.
    if !std::io::stdin().is_terminal() {
      return None;
    }
    match crossterm::terminal::enable_raw_mode() {
      Ok(()) => Some(Self(())),
      Err(e) => {
        tracing::debug!("relay: no raw mode: {e}");
        None
      }
    }
  }
}

impl Drop for RawMode {
  fn drop(&mut self) {
    if let Err(e) = crossterm::terminal::disable_raw_mode() {
      tracing::warn!("cannot restore the terminal: {e}");
    }
  }
}
