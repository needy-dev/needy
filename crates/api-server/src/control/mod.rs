//! What the daemon implements: the domain traits behind the gRPC services,
//! who is calling ([`Caller`]), and the errors ([`ControlError`]) that
//! `protocol::status` maps to gRPC statuses in one table.
//!
//! Nothing here is generated code, except the messages of a runner's own
//! streams (see [`Runners::control`]). The adapters in `protocol::grpc`
//! convert requests into these types, call the traits, and convert the
//! results back.

pub mod clients;
pub mod guest;
pub mod images;
pub mod processes;
pub mod runners;
pub mod runs;

use std::collections::BTreeMap;

use needy_core::event::Event;
use needy_core::types::Principal;

pub use clients::*;
pub use guest::*;
pub use images::*;
pub use processes::*;
pub use runners::*;
pub use runs::*;

/// Who is calling: the listener the call arrived on decides, unless the
/// middleware identified the caller by its bearer token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
  /// A client of the control socket, or one holding the daemon's own
  /// token: the bootstrap and admin credential.
  Operator,
  /// A named paired client with server-granted access. The trace names
  /// it `client:NAME`, including when it has administrator access.
  Client {
    id: String,
    name: String,
    access: ClientAccess,
  },
  /// An actor, on the listener bound to its run. Its calls act on that
  /// run; it may not create, stop or read the logs of runs.
  Actor { run: String },
  /// A runner, by the credential it presented on the control listener. It
  /// reaches `RunnerService.Heartbeat`, `Control` and `Task` only.
  Runner { name: String },
}

impl Caller {
  /// Who the trace says caused what this caller asks for.
  pub fn principal(&self) -> Principal {
    match self {
      Self::Operator => Principal::Operator,
      Self::Client { name, .. } => Principal::Client(name.clone()),
      Self::Actor { run } => Principal::Actor(run.clone()),
      Self::Runner { name } => Principal::Runner(name.clone()),
    }
  }

  /// Whether the caller has an operator's rights over runs and processes:
  /// the operator itself, or a paired client.
  pub fn is_operator(&self) -> bool {
    matches!(self, Self::Operator | Self::Client { .. })
  }
}

/// What a bearer token is to the runner registry or the client registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credential {
  /// The credential of this runner.
  Runner(String),
  /// The token of this paired client.
  Client {
    id: String,
    name: String,
    access: ClientAccess,
  },
  /// Shaped like a runner credential or a client token, but nobody holds
  /// it: removed, revoked, rotated away, or never issued.
  Refused,
  /// Not a credential of this registry.
  Other,
}

/// Safe producer facts. The protocol adapters validate and bound their wire form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
  pub reason: String,
  pub facts: BTreeMap<String, String>,
}

/// Why the daemon refused or failed a call. Each variant is one row of the
/// status table in `protocol::status`.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ControlError {
  /// Original domain cause with safe producer facts.
  #[error("{error}")]
  Diagnostic {
    #[source]
    error: Box<ControlError>,
    diagnostic: Diagnostic,
  },
  /// The caller may not do this, or a rule or an injected fault refused a
  /// command. `events` are the refused command's trace events: the faults
  /// chaos injected and the placement decision.
  #[error("{message}")]
  Denied { message: String, events: Vec<Event> },
  /// A secret in the request, such as a join token, is not valid. A
  /// missing or wrong bearer token is refused by the middleware instead.
  #[error("{0}")]
  Unauthenticated(String),
  #[error("{0}")]
  NotFound(String),
  /// The request is malformed.
  #[error("{0}")]
  Invalid(String),
  /// The request cannot be carried out, such as a run whose profile is
  /// invalid or a command that cannot start.
  #[error("{0}")]
  Failed(String),
  /// A limit is reached, such as a full executor with `on_full: reject`.
  #[error("{0}")]
  Exhausted(String),
  /// Something the call needs is going away, such as a closing executor.
  #[error("{0}")]
  Unavailable(String),
  #[error("{0}")]
  Internal(String),
}

impl ControlError {
  /// Attaches a known producer reason without changing the original cause.
  pub fn with_diagnostic(self, reason: &str, facts: impl IntoIterator<Item = (String, String)>) -> Self {
    Self::Diagnostic {
      error: Box::new(self),
      diagnostic: Diagnostic {
        reason: reason.into(),
        facts: facts.into_iter().collect(),
      },
    }
  }

  /// A known caller/protocol boundary refusal.
  pub fn role_denied(message: impl Into<String>) -> Self {
    Self::denied(message).with_diagnostic("auth.role_denied", [])
  }

  /// A refusal that carries no trace events.
  pub fn denied(message: impl Into<String>) -> Self {
    Self::Denied {
      message: message.into(),
      events: Vec::new(),
    }
  }
}

pub type Result<T, E = ControlError> = std::result::Result<T, E>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowSize {
  pub rows: u32,
  pub cols: u32,
}
