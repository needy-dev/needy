//! The terminal-state model of a process on a terminal: what its screen
//! shows and which input modes it turned on, so a client that attaches, or
//! one that fell behind, gets the current screen drawn instead of a replay
//! of raw bytes that may start inside the alternate screen or an escape
//! sequence.
//!
//! `vt100` keeps the screen, the cursor and most input modes. It does not
//! track focus reporting, the kitty keyboard protocol's flags, xterm's
//! modifyOtherKeys, color scheme reports or the window title; [`Extras`]
//! records those from the sequences `vt100` passes on as unhandled.

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
  /// xterm's modifyOtherKeys level, `CSI > 4 ; level m`; 0 is off.
  modify_other_keys: u16,
  /// DEC private mode 2031: the terminal reports color scheme changes.
  color_scheme_reports: bool,
  /// DEC private mode 7 (DECAWM) turned off: no autowrap.
  no_autowrap: bool,
  /// The cursor color the program set with OSC 12, until it resets it with
  /// OSC 112.
  cursor_color: Option<Vec<u8>>,
  title: Option<Vec<u8>>,
  /// Answers to the queries in the bytes processed last that the model can
  /// answer, a cursor position report with the cursor where its query
  /// found it.
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

  // Source: https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-412/ctlseqs.txt#L2012
  // (`OSC Ps ; Pt ST`: Ps 12 sets the text cursor color, `?` asks for it)
  // and #L2258 (Ps 112 resets it), xterm patch 412.
  fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
    match params {
      [b"12", color, ..] if *color != b"?" => self.cursor_color = Some(color.to_vec()),
      [b"112", ..] => self.cursor_color = None,
      _ => {}
    }
  }

  fn unhandled_csi(&mut self, screen: &mut vt100::Screen, i1: Option<u8>, _i2: Option<u8>, params: &[&[u16]], c: char) {
    let first = params.first().and_then(|p| p.first()).copied();
    match (i1, c) {
      // Source: https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-412/ctlseqs.txt#L1383-L1387
      // (`CSI 6 n`, DSR, is answered `CSI r ; c R`, 1-based), xterm patch 412.
      (None, 'n') if first == Some(6) => {
        let (row, col) = screen.cursor_position();
        self.answers.extend_from_slice(cursor_report(row, col).as_bytes());
      }
      // `CSI 5 n`, the operating status, is answered "OK", `CSI 0 n`.
      // Source: https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-412/ctlseqs.txt#L1383-L1385
      (None, 'n') if first == Some(5) => self.answers.extend_from_slice(b"\x1b[0n"),
      // Primary device attributes (DA1), `CSI c` or `CSI 0 c`: a VT220 with
      // ANSI color, as libghostty answers by default (herdr's terminal
      // model). Programs send DA1 last, after the queries a terminal may
      // ignore (the kitty keyboard protocol, colors, modes), and take its
      // answer as "the others are not supported"; without it, Codex
      // (crossterm) waits for a timeout before it draws.
      // Source: https://github.com/herdrdev/herdr/blob/ee5bfb5041bb3e4a49d591c72308aae3c81ecdde/vendor/libghostty-vt/src/terminal/device_attributes.zig#L37-L74
      // (`Primary`: conformance level 62 (VT220), feature 22 (ANSI color))
      // and #L174-L179 (the default encodes as `CSI ? 62 ; 22 c`).
      // Reference: https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-412/ctlseqs.txt#L769-L798
      // (`CSI Ps c`, Send Device Attributes (Primary DA)), xterm patch 412.
      // NOTICE: queries about the client's own terminal (the kitty keyboard
      // protocol's flags, OSC 10/11 colors, DECRQM, XTVERSION, DA2) are not
      // answered: needy passes the client's input on as it is, so it cannot
      // claim a feature for a terminal that attaches later, which herdr can
      // since it encodes keys again for each pane. A program that starts
      // with no client terminal attached (`run create -t`, `exec -d -t`)
      // therefore runs without them; see the TUI fidelity evidence.
      // TODO(terminal-queries): `CSI ? 6 n` (DECXCPR) is not answered: no
      // program needy runs was seen to send it. Answer it when one does.
      (None, 'c') if first.unwrap_or(0) == 0 => self.answers.extend_from_slice(b"\x1b[?62;22c"),
      // Source: https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-412/ctlseqs.txt#L1006
      // and #L1125 (DEC private mode 1004, set and reset), xterm patch 412.
      (Some(b'?'), 'h' | 'l') => {
        if params.iter().any(|p| p.first() == Some(&1004)) {
          self.focus = c == 'h';
        }
        // Mode 2031, color scheme reports, as herdr's terminal model
        // (libghostty) names it.
        // Source: https://github.com/herdrdev/herdr/blob/ee5bfb5041bb3e4a49d591c72308aae3c81ecdde/vendor/libghostty-vt/src/terminal/modes.zig#L329
        // Reference: https://contour-terminal.org/vt-extensions/color-palette-update-notifications/
        // (retrieved 2026-11-13, the page has no versions).
        if params.iter().any(|p| p.first() == Some(&2031)) {
          self.color_scheme_reports = c == 'h';
        }
        // Source: https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-412/ctlseqs.txt#L926
        // and #L1076 (`Ps = 7`, DECAWM, set and reset), xterm patch 412.
        if params.iter().any(|p| p.first() == Some(&7)) {
          self.no_autowrap = c == 'l';
        }
      }
      // `CSI > 4 ; Pv m` sets modifyOtherKeys to Pv; without Pv it is reset
      // to the terminal's initial value, off.
      // Source: https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-412/ctlseqs.txt#L1328-L1351
      // (XTMODKEYS, `Pp = 4`), xterm patch 412.
      (Some(b'>'), 'm') if first == Some(4) => {
        self.modify_other_keys = params.get(1).and_then(|p| p.first()).copied().unwrap_or(0);
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
/// modifyOtherKeys, color scheme reports, application cursor and keypad
/// keys, the scroll region and the drawing attributes; it turns autowrap
/// on and resets the cursor color.
// Source: https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-412/ctlseqs.txt#L1068-L1185
// (reset `CSI ? Pm l`), xterm patch 412; `CSI r` resets the scroll region,
// `ESC >` the keypad, `CSI > 4 m` modifyOtherKeys (#L1328-L1340), `CSI ? 7 h`
// sets autowrap (#L926), `OSC 112` resets the cursor color (#L2258).
// NOTICE: the program's scroll region and origin mode are not drawn again:
// `vt100` keeps them but does not expose them. A program that relies on a
// scroll region draws correctly again after its next full repaint.
const RESET: &str = "\x1b[?1049l\x1b[=0;1u\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1015l\x1b[?2004l\x1b[?1004l\x1b[>4m\x1b[?2031l\x1b[?7h\x1b]112\x07\x1b[?1l\x1b>\x1b[r\x1b[0m";

impl Screen {
  pub(super) fn new((rows, cols): (u16, u16)) -> Self {
    let (rows, cols) = dimensions(rows, cols);
    Self {
      parser: vt100::Parser::new_with_callbacks(rows, cols, 0, Extras::default()),
    }
  }

  /// Follows `bytes` and returns the answers a terminal would send to the
  /// queries in them the model can answer: the cursor position (DSR 6), the
  /// status (DSR 5) and the primary device attributes (DA1).
  pub(super) fn process(&mut self, bytes: &[u8]) -> Vec<u8> {
    self.parser.process(bytes);
    std::mem::take(&mut self.parser.callbacks_mut().answers)
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
    if extras.modify_other_keys != 0 {
      out.push_str(&format!("\x1b[>4;{}m", extras.modify_other_keys));
    }
    if extras.color_scheme_reports {
      out.push_str("\x1b[?2031h");
    }
    // After the contents, which `vt100` draws relying on autowrap.
    if extras.no_autowrap {
      out.push_str("\x1b[?7l");
    }
    let mut out = out.into_bytes();
    if let Some(color) = &extras.cursor_color {
      out.extend_from_slice(b"\x1b]12;");
      out.extend_from_slice(color);
      out.push(0x07);
    }
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
