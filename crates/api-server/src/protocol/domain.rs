//! Mappings between proto enums and `needy_core` types, one function per
//! direction. The generated types never reach `needy_core`.
//!
//! A proto enum arrives as an `i32`. Its zero value means the sender left
//! the field unset, and a value this build does not know comes from a newer
//! schema; both are refused instead of read as a default.

use needy_api_proto::v1;
use needy_core::types::{Origin, SessionState};

/// A proto enum value with no domain meaning.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
  #[error("the {0} is unspecified")]
  Unspecified(&'static str),
  #[error("unknown {field} {value}")]
  Unknown { field: &'static str, value: i32 },
}

/// The origin an `ExecStart` names.
pub fn origin(value: i32) -> Result<Origin, Error> {
  const FIELD: &str = "exec origin";
  match v1::Origin::try_from(value) {
    Ok(v1::Origin::Unspecified) => Err(Error::Unspecified(FIELD)),
    Ok(v1::Origin::Cli) => Ok(Origin::Cli),
    Ok(v1::Origin::Fs) => Ok(Origin::Fs),
    Ok(v1::Origin::Files) => Ok(Origin::Files),
    Ok(v1::Origin::ActorProxy) => Ok(Origin::ActorProxy),
    Ok(v1::Origin::ActorMount) => Ok(Origin::ActorMount),
    Err(_) => Err(Error::Unknown {
      field: FIELD,
      value,
    }),
  }
}

pub fn proto_origin(origin: Origin) -> v1::Origin {
  match origin {
    Origin::Cli => v1::Origin::Cli,
    Origin::Fs => v1::Origin::Fs,
    Origin::Files => v1::Origin::Files,
    Origin::ActorProxy => v1::Origin::ActorProxy,
    Origin::ActorMount => v1::Origin::ActorMount,
  }
}

/// The state a `Session` names.
pub fn session_state(value: i32) -> Result<SessionState, Error> {
  const FIELD: &str = "session state";
  match v1::SessionState::try_from(value) {
    Ok(v1::SessionState::Unspecified) => Err(Error::Unspecified(FIELD)),
    Ok(v1::SessionState::Creating) => Ok(SessionState::Creating),
    Ok(v1::SessionState::Running) => Ok(SessionState::Running),
    Ok(v1::SessionState::Stopping) => Ok(SessionState::Stopping),
    Ok(v1::SessionState::Stopped) => Ok(SessionState::Stopped),
    Ok(v1::SessionState::Finished) => Ok(SessionState::Finished),
    Ok(v1::SessionState::Failed) => Ok(SessionState::Failed),
    Err(_) => Err(Error::Unknown {
      field: FIELD,
      value,
    }),
  }
}

pub fn proto_session_state(state: SessionState) -> v1::SessionState {
  match state {
    SessionState::Creating => v1::SessionState::Creating,
    SessionState::Running => v1::SessionState::Running,
    SessionState::Stopping => v1::SessionState::Stopping,
    SessionState::Stopped => v1::SessionState::Stopped,
    SessionState::Finished => v1::SessionState::Finished,
    SessionState::Failed => v1::SessionState::Failed,
  }
}

#[cfg(test)]
#[path = "domain_test.rs"]
mod tests;
