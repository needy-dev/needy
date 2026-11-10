use needy_core::types::RunState::*;

use super::*;

#[test]
fn a_run_starts_creating_and_runs() {
  let state = State::new();
  assert_eq!(state.get(), Creating);
  assert_eq!(state.advance(Running), Ok(Creating));
  assert_eq!(state.get(), Running);
}

#[test]
fn a_stop_goes_through_stopping() {
  let state = State::new();
  state.advance(Running).unwrap();
  assert_eq!(state.advance(Stopping), Ok(Running));
  assert_eq!(state.advance(Stopped), Ok(Stopping));
  assert_eq!(state.get(), Stopped);
}

#[test]
fn the_first_terminal_state_wins() {
  let state = State::new();
  state.advance(Running).unwrap();
  state.advance(Finished).unwrap();
  assert_eq!(
    state.advance(Stopping),
    Err(Refused {
      from: Finished,
      to: Stopping
    })
  );
  assert_eq!(state.advance(Stopped).unwrap_err().from, Finished);
  assert_eq!(state.advance(Failed).unwrap_err().from, Finished);
  assert_eq!(state.get(), Finished);
}

#[test]
fn an_actor_exit_during_a_stop_is_refused() {
  let state = State::new();
  state.advance(Running).unwrap();
  state.advance(Stopping).unwrap();
  assert_eq!(state.advance(Finished).unwrap_err().from, Stopping);
  assert_eq!(state.get(), Stopping);
}

#[test]
fn a_creating_run_cannot_stop_but_can_fail() {
  let state = State::new();
  assert_eq!(state.advance(Stopping).unwrap_err().from, Creating);
  assert_eq!(state.advance(Failed), Ok(Creating));
  assert_eq!(state.advance(Running).unwrap_err().from, Failed);
}

#[test]
fn an_actor_that_exits_while_its_run_is_created_finishes_it() {
  let state = State::new();
  assert_eq!(state.advance(Finished), Ok(Creating));
  assert_eq!(state.advance(Running).unwrap_err().from, Finished);
}

#[test]
fn a_running_run_whose_actor_never_reported_an_exit_can_fail() {
  let state = State::new();
  state.advance(Running).unwrap();
  assert_eq!(state.advance(Failed), Ok(Running));
  assert_eq!(state.advance(Stopping).unwrap_err().from, Failed);
  assert_eq!(state.get(), Failed);
}
