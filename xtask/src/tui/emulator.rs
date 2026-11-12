//! The outer terminal the harness plays: a screen (`vt100`) that answers the
//! queries a program sends the way a modern terminal emulator does, and
//! records what the program asked and which modes it left on.
//!
//! `vt100` keeps the screen, the cursor and most input modes but answers no
//! query and passes on DCS strings to nobody, so a second parser (`vte`, the
//! one `vt100` is built on) finds the queries. The screen is brought up to
//! the byte of each query before it is answered, so a cursor position
//! report says where the query found the cursor.

use std::num::NonZeroU16;

use serde_json::{Value, json};
use vte::{Params, Perform};

/// Which queries the terminal answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Answers {
  /// Every query below, as a modern terminal does.
  All,
  /// None: a terminal that never answers, to find programs that wait.
  None,
}

/// What a program asked the terminal.
#[derive(Debug)]
struct Query {
  /// A short name: `DA1`, `DECRQM ?2026`, `OSC 11`, ...
  kind: String,
  /// The answer, empty when the terminal has none.
  reply: Vec<u8>,
}

/// Something in the output to handle once the screen has caught up with it.
#[derive(Debug)]
enum Event {
  Query(Query),
  /// `CSI > flags u`, `CSI < n u`, `CSI = flags ; mode u`.
  Keyboard(char, u16, u16),
}

/// The `vte` side: finds queries, keyboard protocol changes and the modes
/// `vt100` does not track.
#[derive(Default)]
struct Scanner {
  events: Vec<Event>,
  /// DEC private mode 1004.
  focus: bool,
  /// DEC private mode 2026, synchronized output.
  synchronized: bool,
  /// DEC private mode 2031, color scheme change reports.
  color_scheme_reports: bool,
  /// DEC private mode 1016, SGR mouse reports in pixels.
  pixel_mouse: bool,
  /// DEC private mode 7 turned off: no autowrap.
  no_autowrap: bool,
  /// xterm's modifyOtherKeys level (`CSI > 4 ; n m`), 0 when off.
  modify_other_keys: u16,
  /// The cursor color was set (OSC 12) and not reset (OSC 112).
  cursor_color: bool,
  synchronized_begins: u64,
  hyperlinks: u64,
  clipboard_writes: u64,
  notifications: u64,
  dcs: Option<(Vec<u8>, char, Vec<u8>)>,
}

fn first(params: &Params) -> Option<u16> {
  params.iter().next().and_then(|p| p.first()).copied()
}

impl Perform for Scanner {
  fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
    let p0 = first(params);
    let query = |kind: &str| {
      Event::Query(Query {
        kind: kind.into(),
        reply: Vec::new(),
      })
    };
    let event = match (intermediates, action) {
      ([], 'n') if p0 == Some(6) => query("CPR"),
      ([], 'n') if p0 == Some(5) => query("DSR 5"),
      ([b'?'], 'n') if p0 == Some(6) => query("DECXCPR"),
      ([], 'c') if p0.unwrap_or(0) == 0 => query("DA1"),
      ([b'>'], 'c') if p0.unwrap_or(0) == 0 => query("DA2"),
      ([b'='], 'c') if p0.unwrap_or(0) == 0 => query("DA3"),
      ([b'>'], 'q') if p0.unwrap_or(0) == 0 => query("XTVERSION"),
      ([b'?', b'$'], 'p') => query(&format!("DECRQM ?{}", p0.unwrap_or(0))),
      ([b'$'], 'p') => query(&format!("DECRQM {}", p0.unwrap_or(0))),
      ([b'?'], 'u') => query("KITTY ?u"),
      ([], 't') if matches!(p0, Some(14 | 16 | 18)) => query(&format!("XTWINOPS {}", p0.unwrap_or(0))),
      // Source: https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-412/ctlseqs.txt
      // (`CSI > Pp ; Pv m`, XTMODKEYS: Pp 4 is modifyOtherKeys; without Pv
      // the resource is reset), xterm patch 412.
      ([b'>'], 'm') => {
        if p0 == Some(4) {
          self.modify_other_keys = params.iter().nth(1).and_then(|p| p.first()).copied().unwrap_or(0);
        }
        return;
      }
      ([b'>'], 'u') => Event::Keyboard('>', p0.unwrap_or(0), 0),
      ([b'<'], 'u') => Event::Keyboard('<', p0.unwrap_or(1), 0),
      ([b'='], 'u') => {
        let mode = params.iter().nth(1).and_then(|p| p.first()).copied().unwrap_or(1);
        Event::Keyboard('=', p0.unwrap_or(0), mode)
      }
      ([b'?'], 'h' | 'l') => {
        let on = action == 'h';
        for p in params.iter() {
          match p.first() {
            Some(1004) => self.focus = on,
            Some(2026) => {
              if on && !self.synchronized {
                self.synchronized_begins += 1;
              }
              self.synchronized = on;
            }
            Some(2031) => self.color_scheme_reports = on,
            Some(1016) => self.pixel_mouse = on,
            Some(7) => self.no_autowrap = !on,
            _ => {}
          }
        }
        return;
      }
      _ => return,
    };
    self.events.push(event);
  }

  fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
    let Some(code) = params.first() else { return };
    let end: &[u8] = if bell_terminated { b"\x07" } else { b"\x1b\\" };
    match *code {
      b"10" | b"11" | b"12" if params.get(1) == Some(&&b"?"[..]) => {
        // NOTICE: the colors a light-on-dark terminal reports; any fixed
        // value works, the harness only checks that an answer arrives.
        let rgb = match *code {
          b"10" => "rgb:d0d0/d0d0/d0d0",
          b"11" => "rgb:1c1c/1c1c/1c1c",
          _ => "rgb:d0d0/d0d0/d0d0",
        };
        let code = String::from_utf8_lossy(code);
        let mut reply = format!("\x1b]{code};{rgb}").into_bytes();
        reply.extend_from_slice(end);
        self.events.push(Event::Query(Query {
          kind: format!("OSC {code}"),
          reply,
        }));
      }
      b"4" if params.get(2) == Some(&&b"?"[..]) => {
        let index = String::from_utf8_lossy(params[1]);
        let mut reply = format!("\x1b]4;{index};rgb:8080/8080/8080").into_bytes();
        reply.extend_from_slice(end);
        self.events.push(Event::Query(Query {
          kind: "OSC 4".into(),
          reply,
        }));
      }
      // A clipboard read is a query a terminal refuses by default.
      b"52" if params.get(2) == Some(&&b"?"[..]) => {
        self.events.push(Event::Query(Query {
          kind: "OSC 52 read".into(),
          reply: Vec::new(),
        }));
      }
      b"52" => self.clipboard_writes += 1,
      b"12" => self.cursor_color = true,
      b"112" => self.cursor_color = false,
      b"8" if params.get(2).is_some_and(|uri| !uri.is_empty()) => self.hyperlinks += 1,
      b"9" | b"777" => self.notifications += 1,
      _ => {}
    }
  }

  fn hook(&mut self, _params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
    self.dcs = Some((intermediates.to_vec(), action, Vec::new()));
  }

  fn put(&mut self, byte: u8) {
    if let Some((_, _, data)) = self.dcs.as_mut() {
      data.push(byte);
    }
  }

  fn unhook(&mut self) {
    let Some((intermediates, action, data)) = self.dcs.take() else {
      return;
    };
    // Source: https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-412/ctlseqs.txt
    // (`DCS + q Pt ST`, XTGETTCAP, answered `DCS 0 + r Pt ST` when the
    // terminal does not know the capability; `DCS $ q Pt ST`, DECRQSS,
    // answered `DCS 0 $ r Pt ST` when the request is invalid), xterm patch 412.
    let kind = match (intermediates.as_slice(), action) {
      (b"+", 'q') => "XTGETTCAP",
      (b"$", 'q') => "DECRQSS",
      _ => return,
    };
    let reply = match kind {
      "XTGETTCAP" => format!("\x1bP0+r{}\x1b\\", String::from_utf8_lossy(&data)),
      _ => "\x1bP0$r\x1b\\".to_string(),
    };
    self.events.push(Event::Query(Query {
      kind: kind.into(),
      reply: reply.into_bytes(),
    }));
  }
}

/// What `vt100` reports through its callbacks.
#[derive(Default)]
struct Callbacks {
  title: Option<String>,
  bells: u64,
}

impl vt100::Callbacks for Callbacks {
  fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
    self.title = Some(String::from_utf8_lossy(title).into_owned());
  }

  fn audible_bell(&mut self, _: &mut vt100::Screen) {
    self.bells += 1;
  }
}

/// One query and whether the terminal answered it.
struct Asked {
  kind: String,
  answered: bool,
  at_ms: u128,
}

pub struct Emulator {
  screen: vt100::Parser<Callbacks>,
  scanner: Scanner,
  parser: vte::Parser,
  answers: Answers,
  /// The kitty keyboard protocol's flag stacks, main and alternate screen.
  keyboard: [Vec<u16>; 2],
  asked: Vec<Asked>,
  started: std::time::Instant,
}

fn size(rows: u16, cols: u16) -> (NonZeroU16, NonZeroU16) {
  (NonZeroU16::new(rows).unwrap_or(NonZeroU16::MIN), NonZeroU16::new(cols).unwrap_or(NonZeroU16::MIN))
}

impl Emulator {
  pub fn new(rows: u16, cols: u16, answers: Answers) -> Self {
    let (rows, cols) = size(rows, cols);
    Self {
      screen: vt100::Parser::new_with_callbacks(rows, cols, 0, Callbacks::default()),
      scanner: Scanner::default(),
      parser: vte::Parser::new(),
      answers,
      keyboard: [Vec::new(), Vec::new()],
      asked: Vec::new(),
      started: std::time::Instant::now(),
    }
  }

  pub fn screen(&self) -> &vt100::Screen {
    self.screen.screen()
  }

  pub fn resize(&mut self, rows: u16, cols: u16) {
    let (rows, cols) = size(rows, cols);
    self.screen.set_size(rows, cols);
  }

  /// The kitty keyboard protocol's flags on the current screen.
  pub fn keyboard_flags(&self) -> u16 {
    self.keyboard[usize::from(self.screen().alternate_screen())].last().copied().unwrap_or(0)
  }

  pub fn focus(&self) -> bool {
    self.scanner.focus
  }

  /// Follows `bytes` and returns what the terminal answers to them.
  pub fn process(&mut self, bytes: &[u8]) -> Vec<u8> {
    let mut replies = Vec::new();
    let mut start = 0;
    for at in 0..bytes.len() {
      self.parser.advance(&mut self.scanner, &bytes[at..=at]);
      if self.scanner.events.is_empty() {
        continue;
      }
      self.screen.process(&bytes[start..=at]);
      start = at + 1;
      for event in std::mem::take(&mut self.scanner.events) {
        match event {
          Event::Query(query) => {
            let reply = self.reply(query.kind.as_str(), query.reply);
            self.asked.push(Asked {
              kind: query.kind,
              answered: !reply.is_empty(),
              at_ms: self.started.elapsed().as_millis(),
            });
            replies.extend_from_slice(&reply);
          }
          Event::Keyboard(op, value, mode) => self.keyboard(op, value, mode),
        }
      }
    }
    self.screen.process(&bytes[start..]);
    replies
  }

  // NOTICE: the kitty keyboard protocol keeps one stack of flags per screen.
  // Source: https://github.com/kovidgoyal/kitty/blob/v0.50.0/docs/keyboard-protocol.rst?plain=1#L279-L328
  fn keyboard(&mut self, op: char, value: u16, mode: u16) {
    let stack = &mut self.keyboard[usize::from(self.screen.screen().alternate_screen())];
    match op {
      '>' => stack.push(value),
      '<' => {
        let n = usize::from(value.max(1)).min(stack.len());
        stack.truncate(stack.len() - n);
      }
      _ => {
        if stack.is_empty() {
          stack.push(0);
        }
        if let Some(top) = stack.last_mut() {
          *top = match mode {
            2 => *top | value,
            3 => *top & !value,
            _ => value,
          };
        }
      }
    }
  }

  /// The answer to a query of `kind`; `prepared` when the scanner made it.
  // Source: https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-412/ctlseqs.txt
  // (DSR `CSI 6 n` -> `CSI r ; c R`, `CSI ? 6 n` -> `CSI ? r ; c R`,
  // `CSI 5 n` -> `CSI 0 n`; DA1 `CSI ? ... c`; DA2 `CSI > Pp ; Pv ; Pc c`;
  // XTVERSION `DCS > | text ST`; DECRQM `CSI ? Ps ; Pm $ y` with Pm 0 not
  // recognized, 1 set, 2 reset; XTWINOPS 14/16/18 `CSI 4 ; h ; w t`,
  // `CSI 6 ; h ; w t`, `CSI 8 ; rows ; cols t`), xterm patch 412.
  // Reference: https://invisible-island.net/xterm/ctlseqs/ctlseqs.html
  // Source: https://github.com/kovidgoyal/kitty/blob/v0.50.0/docs/keyboard-protocol.rst?plain=1#L330-L340
  // (`CSI ? u` answered `CSI ? flags u`).
  // Reference: https://gist.github.com/christianparpart/d8a62cc1ab659194337d73e399004036
  // (mode 2026, synchronized output, reported by DECRQM).
  fn reply(&self, kind: &str, prepared: Vec<u8>) -> Vec<u8> {
    if self.answers == Answers::None {
      return Vec::new();
    }
    if !prepared.is_empty() {
      return prepared;
    }
    let screen = self.screen.screen();
    let (row, col) = screen.cursor_position();
    let (rows, cols) = screen.size();
    let (rows, cols) = (rows.get(), cols.get());
    let mode = |on: bool| if on { 1 } else { 2 };
    let reply = match kind {
      "CPR" => format!("\x1b[{};{}R", row + 1, col + 1),
      "DECXCPR" => format!("\x1b[?{};{}R", row + 1, col + 1),
      "DSR 5" => "\x1b[0n".into(),
      "DA1" => "\x1b[?62;22c".into(),
      "DA2" => "\x1b[>1;10;0c".into(),
      "DA3" => "\x1bP!|00000000\x1b\\".into(),
      "XTVERSION" => "\x1bP>|needy-tui-harness(1)\x1b\\".into(),
      "KITTY ?u" => format!("\x1b[?{}u", self.keyboard_flags()),
      "XTWINOPS 14" => format!("\x1b[4;{};{}t", u32::from(rows) * 16, u32::from(cols) * 8),
      "XTWINOPS 16" => "\x1b[6;16;8t".into(),
      "XTWINOPS 18" => format!("\x1b[8;{rows};{cols}t"),
      private if private.starts_with("DECRQM ?") => {
        let ps: u16 = private["DECRQM ?".len()..].parse().unwrap_or(0);
        let pm = match ps {
          1 => mode(screen.application_cursor()),
          25 => mode(!screen.hide_cursor()),
          47 | 1047 | 1049 => mode(screen.alternate_screen()),
          1000 => mode(screen.mouse_protocol_mode() == vt100::MouseProtocolMode::PressRelease),
          1002 => mode(screen.mouse_protocol_mode() == vt100::MouseProtocolMode::ButtonMotion),
          1003 => mode(screen.mouse_protocol_mode() == vt100::MouseProtocolMode::AnyMotion),
          1006 => mode(screen.mouse_protocol_encoding() == vt100::MouseProtocolEncoding::Sgr),
          1004 => mode(self.scanner.focus),
          2004 => mode(screen.bracketed_paste()),
          2026 => mode(self.scanner.synchronized),
          _ => 0,
        };
        format!("\x1b[?{ps};{pm}$y")
      }
      ansi if ansi.starts_with("DECRQM ") => format!("\x1b[{};0$y", &ansi["DECRQM ".len()..]),
      _ => String::new(),
    };
    reply.into_bytes()
  }

  /// The screen as text, its cursor and modes, and what it shows in color
  /// and wide characters.
  pub fn snapshot(&self) -> Value {
    let screen = self.screen.screen();
    let (rows, cols) = screen.size();
    let (rows, cols) = (rows.get(), cols.get());
    let lines: Vec<String> = screen.rows(0, cols).map(|l| l.trim_end().to_string()).collect();
    let (mut rgb, mut indexed, mut wide) = (0u64, 0u64, 0u64);
    for row in 0..rows {
      for col in 0..cols {
        let Some(cell) = screen.cell(row, col) else {
          continue;
        };
        for color in [cell.fgcolor(), cell.bgcolor()] {
          match color {
            vt100::Color::Rgb(..) => rgb += 1,
            vt100::Color::Idx(_) => indexed += 1,
            vt100::Color::Default => {}
          }
        }
        if cell.is_wide() {
          wide += 1;
        }
      }
    }
    let (row, col) = screen.cursor_position();
    json!({
      "size": [rows, cols],
      "lines": lines,
      "cursor": [row, col],
      "colors": { "rgb": rgb, "indexed": indexed },
      "wide_cells": wide,
      "title": self.screen.callbacks().title,
      "modes": self.modes(),
    })
  }

  /// The modes a program can leave a terminal in.
  pub fn modes(&self) -> Value {
    let screen = self.screen.screen();
    json!({
      "alternate_screen": screen.alternate_screen(),
      "cursor_hidden": screen.hide_cursor(),
      "bracketed_paste": screen.bracketed_paste(),
      "mouse": format!("{:?}/{:?}", screen.mouse_protocol_mode(), screen.mouse_protocol_encoding()),
      "application_cursor": screen.application_cursor(),
      "application_keypad": screen.application_keypad(),
      "focus": self.scanner.focus,
      "keyboard_flags": self.keyboard_flags(),
      "synchronized": self.scanner.synchronized,
      "color_scheme_reports": self.scanner.color_scheme_reports,
      "pixel_mouse": self.scanner.pixel_mouse,
      "no_autowrap": self.scanner.no_autowrap,
      "modify_other_keys": self.scanner.modify_other_keys,
      "cursor_style": format!("{:?}", screen.cursor_style()),
      "cursor_color": self.scanner.cursor_color,
    })
  }

  /// The modes left on that a shell after the program would notice.
  pub fn leftover_modes(&self) -> Vec<&'static str> {
    let screen = self.screen.screen();
    let mut left = Vec::new();
    let checks = [
      (screen.alternate_screen(), "alternate_screen"),
      (screen.hide_cursor(), "cursor_hidden"),
      (screen.bracketed_paste(), "bracketed_paste"),
      (screen.mouse_protocol_mode() != vt100::MouseProtocolMode::None, "mouse"),
      (screen.application_cursor(), "application_cursor"),
      (self.scanner.focus, "focus"),
      (self.keyboard_flags() != 0, "keyboard_flags"),
      (self.scanner.synchronized, "synchronized"),
      (self.scanner.color_scheme_reports, "color_scheme_reports"),
      (self.scanner.pixel_mouse, "pixel_mouse"),
      (self.scanner.no_autowrap, "no_autowrap"),
      (self.scanner.modify_other_keys != 0, "modify_other_keys"),
      (screen.cursor_style() != vt100::CursorStyle::Default, "cursor_style"),
      (self.scanner.cursor_color, "cursor_color"),
    ];
    for (on, name) in checks {
      if on {
        left.push(name);
      }
    }
    left
  }

  /// What the program asked, in order, and what else it used.
  pub fn report(&self) -> Value {
    let asked: Vec<Value> = self.asked.iter().map(|a| json!({ "kind": a.kind, "answered": a.answered, "at_ms": a.at_ms })).collect();
    json!({
      "queries": asked,
      "synchronized_begins": self.scanner.synchronized_begins,
      "hyperlinks": self.scanner.hyperlinks,
      "clipboard_writes": self.scanner.clipboard_writes,
      "notifications": self.scanner.notifications,
      "bells": self.screen.callbacks().bells,
    })
  }
}
