use needy_api_client::proto::v1;
use needy_core::types::SessionState;

use super::*;

#[test]
fn every_session_state_is_read_from_the_wire() {
  let wire = [
    (v1::SessionState::Creating, SessionState::Creating),
    (v1::SessionState::Running, SessionState::Running),
    (v1::SessionState::Stopping, SessionState::Stopping),
    (v1::SessionState::Stopped, SessionState::Stopped),
    (v1::SessionState::Finished, SessionState::Finished),
    (v1::SessionState::Failed, SessionState::Failed),
  ];

  for (value, expected) in wire {
    assert_eq!(state(value as i32).unwrap(), expected);
  }
}

#[test]
fn an_unspecified_session_state_is_refused() {
  let error = state(v1::SessionState::Unspecified as i32).unwrap_err();

  assert_eq!(error.to_string(), "the session state is unspecified");
}

#[test]
fn an_unknown_session_state_is_refused() {
  let error = state(42).unwrap_err();

  assert_eq!(error.to_string(), "unknown session state 42");
}
