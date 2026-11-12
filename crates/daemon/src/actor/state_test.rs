use needy_core::types::ActorState::*;

use super::*;

#[test]
fn an_actor_has_no_state_until_it_starts() {
  let state = State::default();
  assert_eq!(state.get(), None);
  assert_eq!(
    state.advance(Configured),
    Err(Refused {
      from: None,
      to: Configured
    })
  );
  assert_eq!(state.advance(Starting), Ok(None));
  assert_eq!(state.get(), Some(Starting));
}

#[test]
fn an_actor_is_configured_then_ready_then_exits() {
  let state = State::default();
  state.advance(Starting).unwrap();
  assert_eq!(state.advance(Configured), Ok(Some(Starting)));
  assert_eq!(state.advance(Ready), Ok(Some(Configured)));
  assert_eq!(state.advance(Exited), Ok(Some(Ready)));
  assert_eq!(state.get(), Some(Exited));
}

#[test]
fn an_actor_is_not_ready_before_its_guest_is_configured() {
  let state = State::default();
  state.advance(Starting).unwrap();
  assert_eq!(
    state.advance(Ready),
    Err(Refused {
      from: Some(Starting),
      to: Ready
    })
  );
  assert_eq!(state.get(), Some(Starting));
}

#[test]
fn an_actor_may_exit_from_any_state_it_started() {
  for reached in [
    &[Starting][..],
    &[Starting, Configured],
    &[Starting, Configured, Ready],
  ] {
    let state = State::default();
    for &to in reached {
      state.advance(to).unwrap();
    }
    assert_eq!(state.advance(Exited), Ok(reached.last().copied()), "{reached:?}");
  }
}

#[test]
fn an_exited_actor_stays_exited() {
  let state = State::default();
  state.advance(Starting).unwrap();
  state.advance(Exited).unwrap();
  for to in [Starting, Configured, Ready, Exited] {
    assert_eq!(state.advance(to).unwrap_err().from, Some(Exited));
  }
}

#[test]
fn a_guest_configured_twice_or_ready_twice_is_refused() {
  let state = State::default();
  state.advance(Starting).unwrap();
  state.advance(Configured).unwrap();
  assert_eq!(state.advance(Configured).unwrap_err().from, Some(Configured));
  state.advance(Ready).unwrap();
  assert_eq!(state.advance(Ready).unwrap_err().from, Some(Ready));
  assert_eq!(state.advance(Configured).unwrap_err().to_string(), "a ready actor cannot become configured");
}
