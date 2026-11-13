// The tests that use these helpers run Unix programs and are `cfg(unix)`.
#![cfg_attr(not(unix), allow(dead_code))]

use super::*;

fn exited(code: i32) -> Result<relay::Ended, relay::Error> {
  Ok(relay::Ended::Exited(Exit {
    code: Some(code),
    signal: String::new(),
  }))
}

// ROOT CAUSE:
//
// If a command on a Windows runner exited with `exit /b 256`, `needy exec`
// on a Unix client exited 0 because the CLI passed 256 to
// `process::exit`, which keeps only the low 8 bits there.
//
// Before the fix, 256 was a success and 300 exited 44.
// The fix exits with a failure for a code outside 0-255 and prints the code.
#[cfg(unix)]
#[test]
fn a_windows_exit_code_above_255_is_a_failure_on_a_unix_client() {
  let ctx = Context {
    config: None,
    selected_context: None,
    address: None,
    credential_file: None,
    diagnostic_format: crate::diagnostics::Format::Human,
  };
  assert_eq!(status(exited(256), &ctx).unwrap(), signals::OUT_OF_RANGE);
  assert_eq!(status(exited(300), &ctx).unwrap(), signals::OUT_OF_RANGE);
  assert_eq!(status(exited(-1_073_741_510), &ctx).unwrap(), signals::OUT_OF_RANGE);
  assert_eq!(status(exited(7), &ctx).unwrap(), 7);
  assert_eq!(status(exited(0), &ctx).unwrap(), 0);
}
