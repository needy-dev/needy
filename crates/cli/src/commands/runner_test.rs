use needy_api_client::proto::v1;
use needy_core::types::RunnerRole;

use super::*;

#[test]
fn roles_statuses_and_token_states_are_read_from_the_wire() {
  assert_eq!(role(v1::RunnerRole::Actor as i32).unwrap(), RunnerRole::Actor);
  assert_eq!(role(v1::RunnerRole::Executor as i32).unwrap(), RunnerRole::Executor);
  assert_eq!(runner_status(v1::RunnerStatus::Offline as i32).unwrap(), "offline");
  assert_eq!(token_state(v1::JoinTokenState::UsedUp as i32).unwrap(), "used-up");
}

#[test]
fn unspecified_and_unknown_wire_values_are_refused() {
  assert_eq!(role(v1::RunnerRole::Unspecified as i32).unwrap_err().to_string(), "the runner role is unspecified");
  assert_eq!(runner_status(42).unwrap_err().to_string(), "unknown runner status 42");
  assert_eq!(token_state(0).unwrap_err().to_string(), "the join token state is unspecified");
}

#[test]
fn a_label_is_split_at_its_first_equals_sign() {
  assert_eq!(label("os=darwin").unwrap(), ("os".to_string(), "darwin".to_string()));
  assert_eq!(label("k=a=b").unwrap(), ("k".to_string(), "a=b".to_string()));
  assert_eq!(label("=x").unwrap_err(), "a label is KEY=VALUE, not \"=x\"");
}
