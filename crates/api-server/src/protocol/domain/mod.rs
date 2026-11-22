//! Mappings between generated types and the domain types of `control` and
//! `needy_core`. Local `control::*` ↔ `v1::*` conversions implement `From` /
//! `TryFrom`; foreign types (`needy_core`, `needy_common`, `needy_config`) use
//! free functions. The generated types never reach `needy_core`.
//!
//! A proto enum arrives as an `i32`. Its zero value means the sender left
//! the field unset, and a value this build does not know comes from a newer
//! schema; both are refused instead of read as a default.
//!
//! The daemon relays execs to executors through `needy-api-client`, whose
//! requests are generated types too; [`proto_exec_start`], [`started`] and
//! [`process_info`] are the outbound side of that relay.

pub mod clients;
pub mod guest;
pub mod images;
pub mod processes;
pub mod runners;
pub mod runs;

use needy_api_proto::v1;
use needy_common::process::Exit;
use needy_core::types::{ActorState, Origin, RunState};

use crate::control::{Diagnostic, WindowSize};

pub use clients::*;
pub use guest::*;
pub use images::*;
pub use processes::*;
pub use runners::*;
pub use runs::*;

/// A request field with no domain meaning.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
  #[error("the {0} is unspecified")]
  Unspecified(&'static str),
  #[error("unknown {field} {value}")]
  Unknown { field: &'static str, value: i32 },
  #[error("the request names no {0}")]
  Missing(&'static str),
  #[error("{0}")]
  Contradicts(&'static str),
  #[error("{0}")]
  Invalid(String),
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

// NOTICE: proto -> `RunState` lives on the client side
// (`needy-cli`), the only reader of run states on the wire.
pub fn proto_run_state(state: RunState) -> v1::RunState {
  match state {
    RunState::Creating => v1::RunState::Creating,
    RunState::Running => v1::RunState::Running,
    RunState::Stopping => v1::RunState::Stopping,
    RunState::Stopped => v1::RunState::Stopped,
    RunState::Finished => v1::RunState::Finished,
    RunState::Failed => v1::RunState::Failed,
  }
}

pub fn proto_actor_state(state: Option<ActorState>) -> v1::ActorState {
  match state {
    None => v1::ActorState::Unspecified,
    Some(ActorState::Starting) => v1::ActorState::Starting,
    Some(ActorState::Configured) => v1::ActorState::Configured,
    Some(ActorState::Ready) => v1::ActorState::Ready,
    Some(ActorState::Exited) => v1::ActorState::Exited,
  }
}

pub fn exit(exited: v1::ExecExited) -> Exit {
  Exit {
    code: exited.code,
    signal: exited.signal,
  }
}

pub fn proto_exit(exit: Exit) -> v1::ExecExited {
  v1::ExecExited {
    code: exit.code,
    signal: exit.signal,
  }
}

impl From<v1::WindowSize> for WindowSize {
  fn from(size: v1::WindowSize) -> Self {
    Self {
      rows: size.rows,
      cols: size.cols,
    }
  }
}

impl From<WindowSize> for v1::WindowSize {
  fn from(size: WindowSize) -> Self {
    Self {
      rows: size.rows,
      cols: size.cols,
    }
  }
}

pub fn proto_window(size: WindowSize) -> v1::WindowSize {
  size.into()
}

/// Accepts only the known bounded wire detail; malformed facts are discarded.
pub fn diagnostic(value: v1::ErrorDiagnostic) -> Option<Diagnostic> {
  let value = needy_api_proto::diagnostic::validated(&value)?;
  Some(Diagnostic {
    reason: value.reason,
    facts: value.facts.into_iter().collect(),
  })
}

pub fn proto_diagnostic(value: Diagnostic) -> Option<v1::ErrorDiagnostic> {
  needy_api_proto::diagnostic::validated(&v1::ErrorDiagnostic {
    reason: value.reason,
    facts: value.facts.into_iter().collect(),
  })
}

#[cfg(test)]
#[path = "../domain_test.rs"]
mod tests;
