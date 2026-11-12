use super::*;

/// What a client's terminal shows after it receives `bytes`.
fn client(rows: u16, cols: u16, bytes: &[u8]) -> vt100::Parser {
  let (rows, cols) = dimensions(rows, cols);
  let mut parser = vt100::Parser::new(rows, cols, 0);
  parser.process(bytes);
  parser
}

#[test]
fn redraw_reproduces_an_alternate_screen_frame() {
  let mut screen = Screen::new((10, 40));
  let (rows, cols) = dimensions(10, 40);
  let mut program = vt100::Parser::new(rows, cols, 0);
  let output = b"shell$ top\r\n\x1b[?1049h\x1b[2J\x1b[H\x1b[1mTOP\x1b[0m\x1b[5;10Hrow five\x1b[?2004h\x1b[?1000h\x1b[?1006h\x1b[3;3H";
  screen.process(output);
  program.process(output);

  // A client that was on some other screen, with leftovers on it.
  let mut attached = client(10, 40, b"garbage everywhere\x1b[?1049h junk");
  attached.process(&screen.redraw());

  assert_eq!(attached.screen().contents(), program.screen().contents());
  assert_eq!(attached.screen().cursor_position(), (2, 2));
  assert!(attached.screen().alternate_screen());
  assert!(attached.screen().bracketed_paste());
  assert_eq!(attached.screen().mouse_protocol_mode(), vt100::MouseProtocolMode::PressRelease);
  assert_eq!(attached.screen().mouse_protocol_encoding(), vt100::MouseProtocolEncoding::Sgr);
  assert!(attached.screen().cell(0, 0).is_some_and(vt100::Cell::bold));
}

#[test]
fn redraw_leaves_the_alternate_screen_when_the_program_did() {
  let mut screen = Screen::new((5, 20));
  screen.process(b"\x1b[?1049hfull screen\x1b[?1049lback\r\n");
  let mut attached = client(5, 20, b"\x1b[?1049hstale alternate");
  attached.process(&screen.redraw());
  assert!(!attached.screen().alternate_screen());
  assert_eq!(attached.screen().contents().trim_end(), "back");
}

#[test]
fn redraw_restores_focus_keyboard_flags_and_title() {
  let mut screen = Screen::new((5, 20));
  screen.process(b"\x1b]2;editor\x07\x1b[?1004h\x1b[>1u\x1b[>5u\x1b[<1u\x1b[=2;2u");
  let redraw = String::from_utf8(screen.redraw()).unwrap();
  assert!(redraw.contains("\x1b[?1004h"), "{redraw:?}");
  // Pushed 1, pushed 5, popped one, added 2 to the top: 1 | 2.
  assert!(redraw.ends_with("\x1b[=3;1u\x1b]2;editor\x07"), "{redraw:?}");
}

// ROOT CAUSE:
//
// If a TUI turned on xterm's modifyOtherKeys or color scheme reports
// (Claude Code), turned autowrap off (pi, herdr) or set the cursor color
// (opencode), a client that attached later did not get it, because the
// model tracked none of them, and a redraw for a program that had undone
// them left the client's own setting as it was.
//
// Before the fix, the redraws of these TUIs' screens carried none of these
// (the TUI matrix's `attach` and `viewer` paths against `create`).
// The fix tracks them, resets them first and sets them again when the
// program has them set.
#[test]
fn redraw_restores_the_modes_tuis_set_beyond_input_modes() {
  let set = [
    "\x1b[>4;2m",
    "\x1b[?2031h",
    "\x1b[?7l",
    "\x1b]12;#eeeeee\x07",
  ];
  let mut screen = Screen::new((5, 20));
  let off = String::from_utf8(screen.redraw()).unwrap();
  assert!(off.starts_with(RESET), "{off:?}");
  for reset in ["\x1b[>4m", "\x1b[?2031l", "\x1b[?7h", "\x1b]112\x07"] {
    assert!(RESET.contains(reset), "{reset:?}");
  }
  assert!(set.iter().all(|s| !off.contains(s)), "{off:?}");

  screen.process(set.concat().as_bytes());
  let on = String::from_utf8(screen.redraw()).unwrap();
  assert!(set.iter().all(|s| on.contains(s)), "{on:?}");

  screen.process(b"\x1b[>4m\x1b[?2031l\x1b[?7h\x1b]112\x07");
  let undone = String::from_utf8(screen.redraw()).unwrap();
  assert!(set.iter().all(|s| !undone.contains(s)), "{undone:?}");
}

#[test]
fn keyboard_flags_belong_to_their_screen() {
  let mut screen = Screen::new((5, 20));
  screen.process(b"\x1b[?1049h\x1b[>1u\x1b[?1049l");
  let redraw = String::from_utf8(screen.redraw()).unwrap();
  assert!(!redraw.contains("\x1b[=1;1u"), "{redraw:?}");
}

#[test]
fn a_cursor_position_query_is_answered_with_the_cursor_where_it_was_asked() {
  let mut screen = Screen::new((24, 80));
  // Two queries in one chunk, the cursor moved between them, and a status
  // query (DSR 5) between them.
  let answers = screen.process(b"\x1b[3;5H\x1b[6n\x1b[5n\x1b[10;20Hx\x1b[6n");
  assert_eq!(answers, b"\x1b[3;5R\x1b[0n\x1b[10;21R");
  assert!(screen.process(b"more").is_empty());
}

// ROOT CAUSE:
//
// If a TUI started while no client terminal was attached (`run create -t`),
// it waited for the answer to its primary device attributes query (DA1)
// until its own timeout, because the executor answered only the cursor
// position.
//
// Before the fix, Codex drew its first screen later without answers than
// with them (the TUI matrix's `background` path).
// The fix answers DA1 as libghostty does by default, and leaves the
// queries about the client's terminal unanswered.
#[test]
fn primary_device_attributes_are_answered_and_the_client_terminals_features_are_not() {
  let mut screen = Screen::new((24, 80));
  // Kitty keyboard flags, the background color, synchronized output, the
  // terminal's version and DA2, then DA1 in both its forms.
  let answers = screen.process(b"\x1b[?u\x1b]11;?\x07\x1b[?2026$p\x1b[>q\x1b[>c\x1b[c\x1b[0c");
  assert_eq!(answers, b"\x1b[?62;22c\x1b[?62;22c");
}

#[test]
fn resize_follows_the_terminal() {
  let mut screen = Screen::new((5, 20));
  screen.resize(8, 30);
  screen.process(b"\x1b[8;30Hx");
  let attached = client(8, 30, &screen.redraw());
  assert_eq!(attached.screen().cell(7, 29).map(vt100::Cell::contents), Some("x"));
}

// ROOT CAUSE:
//
// If a client attached while the program was on the alternate screen and
// the program then left it, the client showed its own old main screen
// instead of the program's, because the redraw drew only the alternate
// screen.
//
// Before the fix, the redraw entered the alternate screen on top of whatever
// the client's main screen held.
// The fix draws the program's main screen and cursor first, then enters the
// alternate screen, so a later `ESC[?1049l` reveals the program's main screen.
#[test]
fn leaving_the_alternate_screen_after_an_attach_shows_the_programs_main_screen() {
  let mut screen = Screen::new((6, 30));
  let (rows, cols) = dimensions(6, 30);
  let mut program = vt100::Parser::new(rows, cols, 0);
  let before = b"shell$ ls\r\nfile-a  file-b\r\nshell$ vi\x1b[?1049h\x1b[2J\x1b[H~ editor\x1b[3;2H";
  screen.process(before);
  program.process(before);

  let mut attached = client(6, 30, b"the client's own old main\r\nscreen, more of it");
  attached.process(&screen.redraw());
  assert!(attached.screen().alternate_screen());
  assert_eq!(attached.screen().contents(), program.screen().contents());

  let after = b"\x1b[?1049lafter";
  program.process(after);
  attached.process(after);
  assert!(!attached.screen().alternate_screen());
  assert_eq!(attached.screen().contents(), program.screen().contents());
  assert_eq!(attached.screen().cursor_position(), program.screen().cursor_position());
}
