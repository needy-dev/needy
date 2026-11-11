//! The terminal-state model of a process on a terminal: what its screen
//! shows and which input modes it turned on, so a client that attaches, or
//! one that fell behind, gets the current screen drawn instead of a replay
//! of raw bytes that may start inside the alternate screen or an escape
//! sequence.
//!
//! `vt100` keeps the screen, the cursor and most input modes. It does not
//! track focus reporting, the kitty keyboard protocol's flags or the window
//! title; [`Extras`] records those from the sequences `vt100` passes on as
//! unhandled.

use std::num::NonZeroU16;

/// A process's terminal, fed with the same bytes as its replay buffer.
pub(super) struct Screen {
  parser: vt100::Parser<Extras>,
}

/// What the model tracks beside `vt100`.
#[derive(Default)]
struct Extras {
  /// DEC private mode 1004.
  focus: bool,
  /// The kitty keyboard protocol's flag stacks of the primary and the
  /// alternate screen; the top is the last element.
  keyboard: [Vec<u16>; 2],
  title: Option<Vec<u8>>,
  /// Answers to the cursor position queries in the bytes processed last,
  /// with the cursor where each query found it.
  answers: Vec<u8>,
}

impl Extras {
  fn keyboard_mut(&mut self, screen: &vt100::Screen) -> &mut Vec<u16> {
    &mut self.keyboard[usize::from(screen.alternate_screen())]
  }

  fn keyboard_flags(&self, alternate: bool) -> u16 {
    self.keyboard[usize::from(alternate)].last().copied().unwrap_or(0)
  }
}

// NOTICE: the kitty keyboard protocol keeps one stack of flags per screen:
// `CSI > flags u` pushes, `CSI < n u` pops n entries, `CSI = flags ; mode u`
// sets (1), adds (2) or removes (3) flags on the top entry.
// Source: https://github.com/kovidgoyal/kitty/blob/v0.50.0/docs/keyboard-protocol.rst?plain=1#L279-L328
// The stack is capped as kitty caps it, so a program that only pushes cannot
// grow the model.
const KEYBOARD_STACK: usize = 16;

impl vt100::Callbacks for Extras {
  fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
    self.title = Some(title.to_vec());
  }

  fn unhandled_csi(&mut self, screen: &mut vt100::Screen, i1: Option<u8>, _i2: Option<u8>, params: &[&[u16]], c: char) {
    let first = params.first().and_then(|p| p.first()).copied();
    match (i1, c) {
      // Source: https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-412/ctlseqs.txt#L1383-L1387
      // (`CSI 6 n`, DSR, is answered `CSI r ; c R`, 1-based), xterm patch 412.
      // TODO(terminal-queries): DSR 5 (status), `CSI ? 6 n` (DECXCPR) and
      // DA1 are not answered: no program needy runs was seen to wait for
      // them. Answer them when one does.
      (None, 'n') if first == Some(6) => {
        let (row, col) = screen.cursor_position();
        self.answers.extend_from_slice(cursor_report(row, col).as_bytes());
      }
      // Source: https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-412/ctlseqs.txt#L1006
      // and #L1125 (DEC private mode 1004, set and reset), xterm patch 412.
      (Some(b'?'), 'h' | 'l') => {
        if params.iter().any(|p| p.first() == Some(&1004)) {
          self.focus = c == 'h';
        }
      }
      (Some(b'>'), 'u') => {
        let stack = self.keyboard_mut(screen);
        if stack.len() == KEYBOARD_STACK {
          stack.remove(0);
        }
        stack.push(first.unwrap_or(0));
      }
      (Some(b'<'), 'u') => {
        let stack = self.keyboard_mut(screen);
        let n = usize::from(first.unwrap_or(1).max(1)).min(stack.len());
        stack.truncate(stack.len() - n);
      }
      (Some(b'='), 'u') => {
        let flags = first.unwrap_or(0);
        let mode = params.get(1).and_then(|p| p.first()).copied().unwrap_or(1);
        let stack = self.keyboard_mut(screen);
        if stack.is_empty() {
          stack.push(0);
        }
        if let Some(top) = stack.last_mut() {
          *top = match mode {
            2 => *top | flags,
            3 => *top & !flags,
            _ => flags,
          };
        }
      }
      _ => {}
    }
  }
}

/// Turns off every mode a terminal may have on before a redraw: the
/// alternate screen (its contents are drawn again if the program is on it),
/// mouse reporting, bracketed paste, focus events, the kitty keyboard flags,
/// application cursor and keypad keys, the scroll region and the drawing
/// attributes.
// Source: https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-412/ctlseqs.txt#L1068-L1185
// (reset `CSI ? Pm l`), xterm patch 412; `CSI r` resets the scroll region,
// `ESC >` the keypad.
// NOTICE: the program's scroll region and origin mode are not drawn again:
// `vt100` keeps them but does not expose them. A program that relies on a
// scroll region draws correctly again after its next full repaint.
const RESET: &str =
  "\x1b[?1049l\x1b[=0;1u\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1015l\x1b[?2004l\x1b[?1004l\x1b[?1l\x1b>\x1b[r\x1b[0m";

impl Screen {
  pub(super) fn new((rows, cols): (u16, u16)) -> Self {
    let (rows, cols) = dimensions(rows, cols);
    Self {
      parser: vt100::Parser::new_with_callbacks(rows, cols, 0, Extras::default()),
    }
  }

  /// Follows `bytes` and returns the answers a terminal would send to the
  /// cursor position queries in them.
  pub(super) fn process(&mut self, bytes: &[u8]) -> Vec<u8> {
    self.parser.process(bytes);
    std::mem::take(&mut self.parser.callbacks_mut().answers)
  }

  /// The answer to a cursor position query now.
  pub(super) fn cursor_report(&self) -> Vec<u8> {
    let (row, col) = self.parser.screen().cursor_position();
    cursor_report(row, col).into_bytes()
  }

  pub(super) fn resize(&mut self, rows: u16, cols: u16) {
    let (rows, cols) = dimensions(rows, cols);
    self.parser.set_size(rows, cols);
  }

  /// The bytes that turn any terminal into this one: a reset, the main
  /// screen and the alternate screen when the program is on it, the
  /// screen's contents and cursor, then the program's input modes and title.
  pub(super) fn redraw(&self) -> Vec<u8> {
    let screen = self.parser.screen();
    let extras = self.parser.callbacks();
    let mut out = String::from(RESET);
    if screen.alternate_screen() {
      // The program's main screen goes under the alternate one, so that
      // when the program leaves the alternate screen the client shows the
      // program's main screen rather than its own.
      out.push_str(&main_screen(screen).contents_formatted());
      // `CSI ? 1049 h` saves the cursor and its attributes, as the
      // program's own did, then switches to the cleared alternate screen;
      // the program's `CSI ? 1049 l` restores them.
      // Source: https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-412/ctlseqs.txt#L1026-L1031
      // and #L1168-L1172 (`Ps = 1 0 4 9`, set and reset), xterm patch 412.
      out.push_str("\x1b[?1049h");
    }
    // The contents, the cursor and `vt100`'s input modes (keypad, cursor
    // keys, bracketed paste, mouse, cursor style).
    out.push_str(&screen.state_formatted());
    if extras.focus {
      out.push_str("\x1b[?1004h");
    }
    let flags = extras.keyboard_flags(screen.alternate_screen());
    if flags != 0 {
      out.push_str(&format!("\x1b[={flags};1u"));
    }
    let mut out = out.into_bytes();
    if let Some(title) = &extras.title {
      out.extend_from_slice(b"\x1b]2;");
      out.extend_from_slice(title);
      out.push(0x07);
    }
    out
  }
}

/// The program's main screen while it is on the alternate one, with the
/// cursor and attributes it saved on entering the alternate screen.
// NOTICE: `vt100` 0.18 keeps the main grid but formats only the active one.
// A copy of the screen in a scratch parser leaves the alternate screen the
// way the program will, so its active grid is the main one. Remove when
// `vt100` exposes the main grid.
// Source: https://docs.rs/vt100/0.18.1/src/vt100/screen.rs.html (`grid()`
// picks `alternate_grid` while `MODE_ALTERNATE_SCREEN` is set).
fn main_screen(screen: &vt100::Screen) -> vt100::Screen {
  let (rows, cols) = screen.size();
  let mut scratch = vt100::Parser::new(rows, cols, 0);
  *scratch.screen_mut() = screen.clone();
  scratch.process(b"\x1b[?1049l");
  scratch.screen().clone()
}

/// A cursor position report for `vt100`'s 0-based `row` and `col`.
// NOTICE: the position is absolute; a program in origin mode (DECOM) expects
// it relative to its scroll region, which `vt100` does not expose.
fn cursor_report(row: u16, col: u16) -> String {
  format!("\x1b[{};{}R", u32::from(row) + 1, u32::from(col) + 1)
}

fn dimensions(rows: u16, cols: u16) -> (NonZeroU16, NonZeroU16) {
  (NonZeroU16::new(rows).unwrap_or(NonZeroU16::MIN), NonZeroU16::new(cols).unwrap_or(NonZeroU16::MIN))
}

#[cfg(test)]
#[path = "screen_test.rs"]
mod tests;
