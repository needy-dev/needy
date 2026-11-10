use needy_core::event::{Event, RunStop};

use super::*;

#[test]
fn each_error_has_its_code_and_keeps_its_message() {
  let table = [
    (ControlError::denied("actors cannot stop runs"), Code::PermissionDenied),
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
  let event = Event::RunStop(RunStop {});
  let error = ControlError::Denied {
    message: "fault injected: ECONNREFUSED".into(),
    events: vec![event.clone()],
  };

  let status = status(error);

  let refusal = ExecRefusal::decode(status.details()).unwrap();
  assert_eq!(refusal.events, [domain::proto_exec_event(&event)]);
}
