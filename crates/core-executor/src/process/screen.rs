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

  pub(super) fn process(&mut self, bytes: &[u8]) {
    self.parser.process(bytes);
  }

  pub(super) fn resize(&mut self, rows: u16, cols: u16) {
    let (rows, cols) = dimensions(rows, cols);
    self.parser.set_size(rows, cols);
  }

  /// The bytes that turn any terminal into this one: a reset, the
  /// alternate screen when the program is on it, the screen's contents and
  /// cursor, then the program's input modes and title.
  pub(super) fn redraw(&self) -> Vec<u8> {
    let screen = self.parser.screen();
    let extras = self.parser.callbacks();
    let mut out = String::from(RESET);
    if screen.alternate_screen() {
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

fn dimensions(rows: u16, cols: u16) -> (NonZeroU16, NonZeroU16) {
  (NonZeroU16::new(rows).unwrap_or(NonZeroU16::MIN), NonZeroU16::new(cols).unwrap_or(NonZeroU16::MIN))
}

#[cfg(test)]
#[path = "screen_test.rs"]
mod tests;
