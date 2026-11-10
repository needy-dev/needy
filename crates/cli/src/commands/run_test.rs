use needy_api_client::proto::v1;
use needy_core::types::RunState;

use super::*;

#[test]
fn every_run_state_is_read_from_the_wire() {
  let wire = [
    (v1::RunState::Creating, RunState::Creating),
    (v1::RunState::Running, RunState::Running),
    (v1::RunState::Stopping, RunState::Stopping),
    (v1::RunState::Stopped, RunState::Stopped),
    (v1::RunState::Finished, RunState::Finished),
    (v1::RunState::Failed, RunState::Failed),
  ];

  for (value, expected) in wire {
    assert_eq!(state(value as i32).unwrap(), expected);
  }
}

#[test]
fn an_unspecified_run_state_is_refused() {
  let error = state(v1::RunState::Unspecified as i32).unwrap_err();

  assert_eq!(error.to_string(), "the run state is unspecified");
}

#[test]
fn an_unknown_run_state_is_refused() {
  let error = state(42).unwrap_err();

  assert_eq!(error.to_string(), "unknown run state 42");
}
