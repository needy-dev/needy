use super::*;

// ROOT CAUSE:
//
// If a routed command's program could not start on its executor, the proxy
// exited 126, because it treated the refused start like any other error of
// its own. `needy exec` and a shell exit 127 for a program that cannot start.
//
// Before the fix, every refused start fell through to the proxy's 126.
// The fix maps a start that cannot run to 127 and keeps 126 for a rule or
// fault that refuses it.
#[test]
fn a_routed_program_that_cannot_start_exits_127() {
  assert_eq!(refused(Kind::Failed), Some(127));
}

#[test]
fn a_routed_command_a_rule_or_fault_refuses_exits_126() {
  assert_eq!(refused(Kind::Denied), Some(126));
}

#[test]
fn other_start_errors_are_the_proxys_own() {
  assert_eq!(refused(Kind::Unavailable), None);
}
