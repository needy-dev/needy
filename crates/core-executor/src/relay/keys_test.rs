use super::*;

fn scanner(keys: &str) -> Scanner {
  Scanner::parse(keys).unwrap().unwrap()
}

fn forwarded(forward: &[u8]) -> Scanned {
  Scanned {
    forward: forward.to_vec(),
    detach: false,
  }
}

#[test]
fn ctrl_close_bracket_detaches_and_is_not_forwarded() {
  let mut keys = scanner("ctrl-]");
  assert_eq!(keys.scan(b"ls\r"), forwarded(b"ls\r"));
  assert_eq!(
    keys.scan(b"ab\x1dcd"),
    Scanned {
      forward: b"ab".to_vec(),
      detach: true
    }
  );
}

#[test]
fn the_key_is_recognized_in_kitty_and_modify_other_keys_encodings() {
  for encoded in [&b"\x1b[93;5u"[..], b"\x1b[93;5:1u", b"\x1b[27;5;93~"] {
    let mut keys = scanner("ctrl-]");
    assert!(keys.scan(encoded).detach, "{encoded:?}");
  }
}

#[test]
fn other_escape_sequences_and_a_lone_escape_pass_through() {
  let mut keys = scanner("ctrl-]");
  assert_eq!(keys.scan(b"\x1b[A\x1b[97;5u"), forwarded(b"\x1b[A\x1b[97;5u"));
  // A lone escape is not held for the next read.
  assert_eq!(keys.scan(b"\x1b"), forwarded(b"\x1b"));
}

#[test]
fn keys_inside_a_bracketed_paste_are_forwarded() {
  let mut keys = scanner("ctrl-]");
  assert_eq!(keys.scan(b"\x1b[200~a\x1db"), forwarded(b"\x1b[200~a\x1db"));
  assert_eq!(keys.scan(b"\x1d\x1b[201~"), forwarded(b"\x1d\x1b[201~"));
  assert!(keys.scan(b"\x1d").detach);
}

#[test]
fn a_sequence_of_keys_may_span_reads_and_a_broken_one_is_forwarded() {
  let mut keys = scanner("ctrl-p,ctrl-q");
  assert_eq!(keys.scan(b"x\x10"), forwarded(b"x"));
  assert!(keys.scan(b"\x11").detach);
  assert_eq!(keys.scan(b"\x10"), forwarded(b""));
  assert_eq!(keys.scan(b"a"), forwarded(b"\x10a"));
}

#[test]
fn an_empty_string_turns_detaching_off_and_bad_keys_are_refused() {
  assert!(Scanner::parse("").unwrap().is_none());
  assert!(Scanner::parse("ctrl-1").is_err());
  assert!(Scanner::parse("ab").is_err());
  assert!(Scanner::parse("ctrl-a,").is_err());
}
