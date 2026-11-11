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
