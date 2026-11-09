//! The local terminal: whether there is one, its window size, and raw mode
//! for the length of a relay (`crossterm`).

use std::io::{IsTerminal, Write};
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

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

/// The local terminal in raw mode; dropping it restores the terminal with
/// [`restore`].
pub struct RawMode(());

/// A [`RawMode`] is on and [`restore`] has not run for it.
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// The program's output last switched to the alternate screen.
static ALTERNATE: AtomicBool = AtomicBool::new(false);

impl RawMode {
  /// Raw mode, or `None` when stdin is not a terminal to put in it. A panic
  /// restores the terminal before its message is printed.
  pub fn enable() -> Option<Self> {
    // crossterm would fall back to the controlling terminal; a relay whose
    // stdin is a pipe leaves that terminal alone.
    if !std::io::stdin().is_terminal() {
      return None;
    }
    match crossterm::terminal::enable_raw_mode() {
      Ok(()) => {
        ALTERNATE.store(false, Ordering::SeqCst);
        ACTIVE.store(true, Ordering::SeqCst);
        static HOOK: Once = Once::new();
        HOOK.call_once(|| {
          let previous = std::panic::take_hook();
          std::panic::set_hook(Box::new(move |info| {
            restore();
            previous(info);
          }));
        });
        Some(Self(()))
      }
      Err(e) => {
        tracing::debug!("relay: no raw mode: {e}");
        None
      }
    }
  }
}

impl Drop for RawMode {
  fn drop(&mut self) {
    restore();
  }
}

/// Follows the program's output for what [`restore`] must undo that is not
/// safe to undo blindly: the alternate screen, whose reset also restores a
/// saved cursor position. `tail` keeps the end of the previous chunk, so a
/// sequence split between chunks is seen.
#[derive(Default)]
pub struct Modes {
  tail: Vec<u8>,
}

impl Modes {
  pub fn observe(&mut self, bytes: &[u8]) {
    // NOTICE: DEC private modes 1049, 1047 and 47 select the alternate
    // screen (`h`) or the normal one (`l`), per xterm's control sequences
    // (https://invisible-island.net/xterm/ctlseqs/ctlseqs.html). This is not
    // a terminal model: the last switch seen wins.
    // TODO(terminal-model): the terminal-state model of the terminal note
    // (G1) tracks every mode; this scanner goes when it lands.
    let mut text = std::mem::take(&mut self.tail);
    text.extend_from_slice(bytes);
    let mut last = None;
    for at in 0..text.len() {
      for (sequence, on) in SWITCHES {
        if text[at..].starts_with(sequence) {
          last = Some(*on);
        }
      }
    }
    if let Some(on) = last {
      ALTERNATE.store(on, Ordering::SeqCst);
    }
    let keep = text.len().saturating_sub(LONGEST_SWITCH - 1);
    self.tail = text[keep..].to_vec();
  }
}

const SWITCHES: &[(&[u8], bool)] = &[
  (b"\x1b[?1049h", true),
  (b"\x1b[?1049l", false),
  (b"\x1b[?1047h", true),
  (b"\x1b[?1047l", false),
  (b"\x1b[?47h", true),
  (b"\x1b[?47l", false),
];
const LONGEST_SWITCH: usize = 8;

/// Gives the terminal back as it was before the relay: turns off what a
/// program may have turned on (the alternate screen, mouse reporting,
/// bracketed paste, focus events, the kitty keyboard protocol), shows the
/// cursor and leaves raw mode. It runs once per [`RawMode`], from whichever
/// comes first: the relay's end, a panic or a termination signal.
pub fn restore() {
  if !ACTIVE.swap(false, Ordering::SeqCst) {
    return;
  }
  if std::io::stdout().is_terminal() {
    let mut stdout = std::io::stdout();
    if let Err(e) = write_restore(&mut stdout, ALTERNATE.load(Ordering::SeqCst)) {
      tracing::debug!("relay: cannot reset the terminal's modes: {e}");
    }
  }
  if let Err(e) = crossterm::terminal::disable_raw_mode() {
    tracing::warn!("cannot restore the terminal: {e}");
  }
}

/// Writes the sequences that turn the program's modes off; `alternate` when
/// it left the terminal on the alternate screen.
fn write_restore(out: &mut impl Write, alternate: bool) -> std::io::Result<()> {
  // NOTICE: crossterm offers the mouse, paste, focus and keyboard commands
  // only with its `events` feature, which needs `mio` and `signal-hook`; the
  // sequences are written here as xterm documents them
  // (https://invisible-island.net/xterm/ctlseqs/ctlseqs.html, DEC private
  // modes): 1000, 1002, 1003, 1005, 1006 and 1015 mouse reporting, 2004
  // bracketed paste, 1004 focus events. `CSI = 0 ; 1 u` sets the kitty
  // keyboard protocol's flags to none on the current screen
  // (https://sw.kovidgoyal.net/kitty/keyboard-protocol/#progressive-enhancement);
  // it is sent on both screens, and terminals without the protocol ignore it.
  const KEYBOARD: &str = "\x1b[=0;1u";
  out.write_all(KEYBOARD.as_bytes())?;
  if alternate {
    crossterm::queue!(out, crossterm::terminal::LeaveAlternateScreen)?;
    out.write_all(KEYBOARD.as_bytes())?;
  }
  out.write_all(b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1015l\x1b[?2004l\x1b[?1004l")?;
  crossterm::queue!(out, crossterm::cursor::Show)?;
  out.flush()
}

#[cfg(test)]
#[path = "terminal_test.rs"]
mod tests;
