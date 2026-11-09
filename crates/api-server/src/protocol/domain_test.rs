use needy_api_proto::v1;
use needy_core::types::{Origin, SessionState};

use super::*;

#[test]
fn every_origin_survives_the_wire() {
  for value in [
    Origin::Cli,
    Origin::Fs,
    Origin::Files,
    Origin::ActorProxy,
    Origin::ActorMount,
  ] {
    assert_eq!(origin(proto_origin(value) as i32).unwrap(), value);
  }
}

#[test]
fn an_unspecified_origin_is_refused() {
  let error = origin(v1::Origin::Unspecified as i32).unwrap_err();

  assert_eq!(error.to_string(), "the exec origin is unspecified");
}

#[test]
fn an_unknown_origin_is_refused() {
  let error = origin(42).unwrap_err();

  assert_eq!(error.to_string(), "unknown exec origin 42");
}

#[test]
fn every_session_state_survives_the_wire() {
  for state in [
    SessionState::Creating,
    SessionState::Running,
    SessionState::Stopping,
    SessionState::Stopped,
    SessionState::Finished,
    SessionState::Failed,
  ] {
    assert_eq!(session_state(proto_session_state(state) as i32).unwrap(), state);
  }
}

#[test]
fn an_unspecified_session_state_is_refused() {
  let error = session_state(v1::SessionState::Unspecified as i32).unwrap_err();

  assert_eq!(error.to_string(), "the session state is unspecified");
}

#[test]
fn an_unknown_session_state_is_refused() {
  let error = session_state(42).unwrap_err();

  assert_eq!(error.to_string(), "unknown session state 42");
}
