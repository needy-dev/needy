use super::*;

#[test]
fn a_refused_exec_is_denied_and_a_command_that_cannot_start_failed() {
  let refused = Error::from(tonic::Status::permission_denied("denied: no deleting here"));
  let cannot_start = Error::from(tonic::Status::failed_precondition("cannot start nope"));

  assert_eq!(refused.kind(), Kind::Denied);
  assert_eq!(refused.to_string(), "denied: no deleting here");
  assert_eq!(cannot_start.kind(), Kind::Failed);
}

#[test]
fn only_an_unavailable_server_is_worth_retrying() {
  assert!(Error::from(tonic::Status::unavailable("connection refused")).is_transient());
  assert!(!Error::InvalidEndpoint("ftp://example.com".into()).is_transient());
  assert!(!Error::from(tonic::Status::unauthenticated("missing token")).is_transient());
}

#[test]
fn a_failed_command_reports_its_error_output_or_its_exit() {
  let with_stderr = Error::Failed {
    exit: Exit {
      code: Some(2),
      signal: String::new(),
    },
    stderr: "tar: nope: Cannot open\n".into(),
  };
  let silent = Error::Failed {
    exit: Exit {
      code: None,
      signal: "SIGKILL".into(),
    },
    stderr: String::new(),
  };

  assert_eq!(with_stderr.to_string(), "tar: nope: Cannot open");
  assert_eq!(silent.to_string(), "killed by SIGKILL");
}

// A network failure alone cannot prove whether a dispatched command ran.
#[test]
fn unavailable_is_not_proof_that_a_command_was_not_started() {
  let ambiguous = super::Error::from(tonic::Status::unavailable("response lost"));
  assert!(!ambiguous.not_started());
  let confirmed = super::Error::from(crate::diagnostic::with_status(
    tonic::Status::unavailable("wait deadline expired"),
    needy_api_proto::v1::ErrorDiagnostic {
      reason: "process.not_started".into(),
      facts: Default::default(),
    },
  ));
  assert!(confirmed.not_started());
}

#[test]
fn a_closed_invocation_can_confirm_that_a_late_command_never_started() {
  let confirmed = super::Error::from(crate::diagnostic::with_status(
    tonic::Status::failed_precondition("the invocation ended"),
    needy_api_proto::v1::ErrorDiagnostic {
      reason: "process.not_started".into(),
      facts: Default::default(),
    },
  ));
  assert!(confirmed.not_started());
}
