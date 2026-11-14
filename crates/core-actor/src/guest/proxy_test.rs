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
  assert_eq!(refused(&needy_api_client::Error::from(tonic::Status::failed_precondition("start failed"))), Some(127));
}

#[test]
fn a_routed_command_a_rule_or_fault_refuses_exits_126() {
  assert_eq!(refused(&needy_api_client::Error::from(tonic::Status::permission_denied("denied"))), Some(126));
}

#[test]
fn a_lost_start_response_does_not_claim_that_the_command_never_ran() {
  assert_eq!(refused(&needy_api_client::Error::from(tonic::Status::unavailable("lost response"))), Some(255));
}

#[test]
fn a_confirmed_pre_dispatch_timeout_exits_125() {
  let status = needy_api_client::diagnostic::with_status(
    tonic::Status::unavailable("not dispatched"),
    needy_api_proto::v1::ErrorDiagnostic {
      reason: "process.not_started".into(),
      facts: Default::default(),
    },
  );
  assert_eq!(refused(&status.into()), Some(125));
}

fn strings(words: &[&str]) -> Vec<String> {
  words.iter().map(|w| w.to_string()).collect()
}

// ROOT CAUSE:
//
// If the actor ran a routed command it does not have, such as `python3`,
// the kernel handed the proxy the route stub's path, `/run/needy/bin/python3`,
// and the proxy put that path in argv[0], because it reports the path the
// original `execve` named.
//
// Before the fix, the decision, the trace and the report showed
// `/run/needy/bin/python3 -c …`, a path the user never typed.
// The fix names a stub's command by its bare name, as the user invoked it.
#[test]
fn a_command_routed_through_a_stub_keeps_its_bare_name() {
  let argv = command("/run/needy/bin/python3", &strings(&["python3", "-c", "print(1)"]));

  assert_eq!(argv, strings(&["python3", "-c", "print(1)"]));
}

#[test]
fn a_command_the_actor_has_keeps_the_path_it_ran() {
  let argv = command("/usr/bin/git", &strings(&["git", "status"]));

  assert_eq!(argv, strings(&["/usr/bin/git", "status"]));
}

// A protocol error may be reported after the executor accepted a request,
// before the proxy receives Started. It cannot be called a policy refusal.
#[test]
fn a_protocol_failure_after_dispatch_is_an_unknown_outcome() {
  let error = needy_api_client::Error::from(tonic::Status::internal("invalid first task response"));
  assert_eq!(refused(&error), Some(255));
}
