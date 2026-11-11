use super::*;

#[test]
fn names_and_numbers() {
  assert_eq!(number("SIGTERM"), Some(15));
  assert_eq!(name(9), "SIGKILL");
  assert_eq!(parse("TERM").as_deref(), Some("SIGTERM"));
  assert_eq!(parse("2").as_deref(), Some("SIGINT"));
  assert_eq!(parse("NOPE"), None);
  let exit = Exit {
    code: None,
    signal: "SIGTERM".into(),
  };
  assert_eq!(shell_code(&exit), 143);
}

#[cfg(unix)]
#[test]
fn platform_numbers_differ() {
  // SIGUSR1 is 10 on Linux and 30 on macOS; names are what travel.
  let usr1 = number("SIGUSR1").unwrap();
  assert_eq!(name(usr1), "SIGUSR1");
}

// ROOT CAUSE:
//
// If a Windows command exited with a code outside 0-255, such as
// `exit /b 256`, a client on Unix exited 0, a false success, because
// `shell_code` passed the code to `process::exit`, which keeps only its low
// 8 bits on Unix.
//
// Before the fix, 256 became 0 and 300 became 44.
// The fix keeps a code that fits and reports any other as a failure.
#[cfg(unix)]
#[test]
fn a_code_outside_an_exit_status_is_a_failure_not_its_low_bits() {
  let exit = |code| Exit {
    code: Some(code),
    signal: String::new(),
  };
  assert_eq!(shell_code(&exit(0)), 0);
  assert_eq!(shell_code(&exit(255)), 255);
  assert_eq!(unrepresentable(&exit(255)), None);
  for code in [256, 300, 512, -1, -1_073_741_510] {
    assert_eq!(shell_code(&exit(code)), OUT_OF_RANGE, "{code}");
    assert_eq!(unrepresentable(&exit(code)), Some(code));
  }
}
