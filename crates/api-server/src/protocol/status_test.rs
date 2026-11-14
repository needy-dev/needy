use needy_core::event::{Event, RunStop};

use super::*;

#[test]
fn each_error_has_its_code_and_keeps_its_message() {
  let table = [
    (ControlError::denied("actors cannot stop runs"), Code::PermissionDenied),
    (ControlError::Unauthenticated("the join token is not valid".into()), Code::Unauthenticated),
    (ControlError::NotFound("no such run".into()), Code::NotFound),
    (ControlError::Invalid("the exec origin is unspecified".into()), Code::InvalidArgument),
    (ControlError::Failed("invalid profile: x".into()), Code::FailedPrecondition),
    (ControlError::Exhausted("executor is full".into()), Code::ResourceExhausted),
    (ControlError::Unavailable("executor closed".into()), Code::Unavailable),
    (ControlError::Internal("cannot read console.log".into()), Code::Internal),
  ];

  for (error, code) in table {
    let message = error.to_string();
    let status = status(error);
    assert_eq!((status.code(), status.message()), (code, message.as_str()));
  }
}

#[test]
fn a_denied_command_carries_its_events_as_an_exec_refusal() {
  let event = Event::RunStop(RunStop::default());
  let error = ControlError::Denied {
    message: "fault injected: ECONNREFUSED".into(),
    events: vec![event.clone()],
  };

  let status = status(error);

  let refusal = ExecRefusal::decode(status.details()).unwrap();
  assert_eq!(refusal.events, [domain::proto_exec_event(&event)]);
}

#[test]
fn diagnostics_and_raw_refusal_events_survive_both_runner_bridge_directions() {
  use needy_api_client::{Error, diagnostic};
  let event = Event::RunStop(RunStop::default());
  let original = status(ControlError::Diagnostic {
    error: Box::new(ControlError::Denied {
      message: "command refused".into(),
      events: vec![event.clone()],
    }),
    diagnostic: crate::control::Diagnostic {
      reason: "policy.exec_denied".into(),
      facts: Default::default(),
    },
  });
  let bytes = original.details().to_vec();
  let returned = diagnostic::task_status(diagnostic::task_error(&original));
  assert_eq!(returned.code(), Code::PermissionDenied);
  assert_eq!(returned.message(), "command refused");
  assert_eq!(returned.details(), bytes);
  let error = Error::from(returned);
  assert_eq!(error.refusal().unwrap(), [domain::proto_exec_event(&event)]);
  assert_eq!(error.diagnostic().unwrap().reason, "policy.exec_denied");
}

#[test]
fn malformed_or_oversized_diagnostics_keep_the_original_status() {
  use needy_api_client::diagnostic;
  let mut original = Status::failed_precondition("cannot start");
  original.metadata_mut().insert_bin(needy_api_proto::diagnostic::METADATA_KEY, tonic::metadata::MetadataValue::from_bytes(b"not protobuf"));
  assert!(diagnostic::from_status(&original).is_none());
  let huge = needy_api_proto::v1::ErrorDiagnostic {
    reason: "process.cwd_invalid".into(),
    facts: [("cwd".into(), "x".repeat(1025))].into(),
  };
  let result = diagnostic::with_status(original, huge);
  assert_eq!(result.code(), Code::FailedPrecondition);
  assert_eq!(result.message(), "cannot start");
  assert!(diagnostic::from_status(&result).is_none());
}

#[test]
fn the_runner_bridge_drops_unknown_facts_and_arbitrary_metadata() {
  use needy_api_client::diagnostic;
  let mut original = diagnostic::with_status(
    Status::failed_precondition("cannot start"),
    needy_api_proto::v1::ErrorDiagnostic {
      reason: "process.cwd_invalid".into(),
      facts: [
        ("cwd".into(), "/home/user/project".into()),
        ("credential".into(), "private-value".into()),
      ]
      .into(),
    },
  );
  original.metadata_mut().insert("unrelated-secret", "private-value".parse().unwrap());

  let returned = diagnostic::task_status(diagnostic::task_error(&original));

  assert_eq!(returned.code(), Code::FailedPrecondition);
  assert_eq!(returned.message(), "cannot start");
  assert!(returned.metadata().get("unrelated-secret").is_none());
  assert_eq!(diagnostic::from_status(&returned).unwrap().facts, [("cwd".into(), "/home/user/project".into())].into());
}

#[test]
fn an_unknown_diagnostic_reason_retains_the_status_without_forwarding_facts() {
  use needy_api_client::diagnostic;
  let original = diagnostic::with_status(
    Status::unavailable("executor failed"),
    needy_api_proto::v1::ErrorDiagnostic {
      reason: "future.failure".into(),
      facts: [("credential".into(), "private-value".into())].into(),
    },
  );

  let returned = diagnostic::task_status(diagnostic::task_error(&original));

  assert_eq!(returned.code(), Code::Unavailable);
  assert_eq!(returned.message(), "executor failed");
  let value = diagnostic::from_status(&returned).unwrap();
  assert_eq!(value.reason, "future.failure");
  assert!(value.facts.is_empty());
}
