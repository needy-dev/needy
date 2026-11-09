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
