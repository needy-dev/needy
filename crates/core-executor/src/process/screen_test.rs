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

#[test]
fn keyboard_flags_belong_to_their_screen() {
  let mut screen = Screen::new((5, 20));
  screen.process(b"\x1b[?1049h\x1b[>1u\x1b[?1049l");
  let redraw = String::from_utf8(screen.redraw()).unwrap();
  assert!(!redraw.contains("\x1b[=1;1u"), "{redraw:?}");
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
