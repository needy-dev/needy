use super::*;

const INSTANCE_STATES: [InstanceState; 4] = [
  InstanceState::Running,
  InstanceState::Released,
  InstanceState::Ended,
  InstanceState::Lost,
];
const PROCESS_STATES: [ProcessState; 4] = [
  ProcessState::Running,
  ProcessState::Exited,
  ProcessState::Ended,
  ProcessState::Lost,
];

#[test]
fn a_running_hold_ends_once_in_any_end_state() {
  use InstanceState::*;
  for to in [Released, Ended, Lost] {
    assert!(InstanceState::allowed(Running, to), "running -> {}", to.as_str());
  }
  assert!(!InstanceState::allowed(Running, Running));
}

#[test]
fn an_ended_hold_stays_as_it_ended() {
  for from in [
    InstanceState::Released,
    InstanceState::Ended,
    InstanceState::Lost,
  ] {
    for to in INSTANCE_STATES {
      assert!(!InstanceState::allowed(from, to), "{} -> {}", from.as_str(), to.as_str());
    }
  }
}

#[test]
fn a_running_process_ends_once_in_any_end_state() {
  use ProcessState::*;
  for to in [Exited, Ended, Lost] {
    assert!(ProcessState::allowed(Running, to), "running -> {}", to.as_str());
  }
  assert!(!ProcessState::allowed(Running, Running));
}

#[test]
fn an_ended_process_stays_as_it_ended() {
  for from in [
    ProcessState::Exited,
    ProcessState::Ended,
    ProcessState::Lost,
  ] {
    for to in PROCESS_STATES {
      assert!(!ProcessState::allowed(from, to), "{} -> {}", from.as_str(), to.as_str());
    }
  }
}

#[test]
fn a_joined_runner_is_removed_once() {
  use RunnerState::*;
  assert!(RunnerState::allowed(Joined, Removed));
  assert!(!RunnerState::allowed(Removed, Removed));
  assert!(!RunnerState::allowed(Removed, Joined), "a removed runner's name joins as a new runner");
  assert!(!RunnerState::allowed(Joined, Joined));
}

#[test]
fn the_stored_words_are_the_serde_names() {
  for state in INSTANCE_STATES {
    assert_eq!(serde_json::to_value(state).unwrap(), state.as_str());
  }
  for state in PROCESS_STATES {
    assert_eq!(serde_json::to_value(state).unwrap(), state.as_str());
  }
}

#[test]
fn a_refusal_names_the_object_and_both_states() {
  let refused = Refused {
    object: "instance",
    id: "r1:actor".into(),
    from: InstanceState::Released.as_str(),
    to: InstanceState::Lost.as_str(),
  };
  assert_eq!(refused.to_string(), "instance r1:actor is released; it cannot become lost");
}
