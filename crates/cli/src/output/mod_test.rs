use super::*;

// ROOT CAUSE:
//
// If the user detached from a program whose prompt left the cursor
// mid-line, the detach message was printed after the prompt, because it
// started where the cursor was.
//
// Before the fix, the message was `needy: detached ...` at the cursor.
// The fix starts it on a line of its own.
#[test]
fn a_message_after_a_terminal_session_starts_on_its_own_line() {
  assert_eq!(after_terminal_line("detached from process p1"), "\r\nneedy: detached from process p1\n");
}

#[test]
fn sizes_are_shown_in_binary_units() {
  assert_eq!(bytes(512), "512 B");
  assert_eq!(bytes(1536), "1.5 KiB");
  assert_eq!(bytes(600 << 20), "600.0 MiB");
  assert_eq!(bytes(3 << 30), "3.0 GiB");
}
