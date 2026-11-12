use super::*;

fn restored(alternate: bool) -> String {
  let mut out = Vec::new();
  write_restore(&mut out, alternate).unwrap();
  String::from_utf8(out).unwrap()
}

#[test]
fn the_alternate_screen_is_left_only_when_the_program_is_on_it() {
  // Leaving it also restores a saved cursor position, which would move the
  // cursor of a program that never switched.
  assert!(restored(true).contains("\x1b[?1049l"));
  assert!(!restored(false).contains("\x1b[?1049l"));
}

// ROOT CAUSE:
//
// If a client detached from a TUI (or was killed) that had turned on
// modifyOtherKeys, color scheme reports, a cursor shape or color, SGR pixel
// mouse reports or autowrap off, the shell after it kept them, because the
// restore undid only the alternate screen and the input modes.
//
// Before the fix, detaching from Claude Code left modifyOtherKeys and color
// scheme reports on, from opencode its cursor shape and color, from pi
// autowrap off (the TUI matrix's `detach` path).
// The fix turns those off too, as herdr's restore of its own terminal does.
#[test]
fn the_restore_turns_off_what_tuis_turn_on_beyond_the_input_modes() {
  let restore = restored(false);
  for reset in [
    "\x1b[?1016l",
    "\x1b[?9l",
    "\x1b[?2031l",
    "\x1b[?7h",
    "\x1b[>4m",
    "\x1b[0 q",
    "\x1b]112\x07",
  ] {
    assert!(restore.contains(reset), "{reset:?} missing from {restore:?}");
  }
}

#[test]
fn a_switch_split_between_chunks_is_seen() {
  let mut modes = Modes::default();

  modes.observe(b"hello\x1b[?10");
  modes.observe(b"49hscreen");
  let on = ALTERNATE.load(Ordering::SeqCst);
  modes.observe(b"\x1b[?1049l");
  let off = ALTERNATE.load(Ordering::SeqCst);

  assert!(on && !off);
}
