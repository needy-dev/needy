//! Errors of the client: connecting, the daemon's statuses mapped to a
//! [`Kind`], and exec streams that end early.

use needy_common::process::Exit;

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("invalid endpoint: {0}")]
  InvalidEndpoint(String),
  #[error("token is not a valid header value")]
  InvalidToken,
  /// A fact observed at the external connector process boundary.
  #[error("connector {program}: {message}")]
  Connector {
    reason: &'static str,
    program: String,
    message: String,
    exit_code: Option<i32>,
  },
  #[error(transparent)]
  Transport(#[from] tonic::transport::Error),
  /// The server did not answer its health check in time.
  #[error("cannot reach {endpoint}: {message}")]
  Unreachable { endpoint: String, message: String },
  #[error(
    "health identity mismatch: expected role {expected_role} and instance {expected_instance}; received role {role} and instance {instance}"
  )]
  IdentityMismatch {
    expected_role: String,
    expected_instance: String,
    role: String,
    instance: String,
  },
  /// The server refused or ended a call. The status is not a `source`, so a
  /// chained report shows its message once.
  #[error("{}", .0.message())]
  Status(tonic::Status),
  /// The executor reported an error instead of an exit.
  #[error("{0}")]
  Remote(String),
  /// In-band error retaining the additive producer facts.
  #[error("{message}")]
  RemoteDiagnostic {
    message: String,
    diagnostic: needy_api_proto::v1::ErrorDiagnostic,
  },
  /// The exec stream ended before the process exited, so the command's
  /// outcome is unknown. `reason` is why, when the daemon said so.
  #[error("the connection ended before the command exited{}", .reason.as_deref().map(|r| format!(": {r}")).unwrap_or_default())]
  Ended { reason: Option<String> },
  /// The exec stream's input is closed: the stream has ended.
  #[error("the exec stream is closed")]
  Closed,
  /// A command run to completion exited unsuccessfully; `stderr` is its
  /// error output.
  #[error("{}", failed(.exit, .stderr))]
  Failed { exit: Exit, stderr: String },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

fn failed(exit: &Exit, stderr: &str) -> String {
  let stderr = stderr.trim();
  if !stderr.is_empty() {
    return stderr.to_string();
  }
  match exit.code {
    Some(code) => format!("exited with {code}"),
    None => format!("killed by {}", exit.signal),
  }
}

/// What a refused or failed call means to a caller, independent of gRPC.
/// The server maps its errors to statuses in one table
/// (`needy_api_server::protocol::status`); this is its inverse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
  /// No token, or a wrong one.
  Unauthenticated,
  /// A rule, an injected fault or the caller's role refused the request.
  Denied,
  NotFound,
  /// The request is malformed.
  Invalid,
  /// The request cannot be carried out; for an exec, the command cannot
  /// start.
  Failed,
  /// A limit is reached, such as an executor's `max_procs`.
  Exhausted,
  /// The server is not reachable or is going away.
  Unavailable,
  /// An exec stream ended before its command exited: the daemon ended it,
  /// for example because its run stopped, or the connection closed.
  Ended,
  /// Anything else: a bug, or a status this build does not map.
  Other,
}

impl Error {
  /// Bounded producer facts, when available; the original cause stays intact.
  pub fn diagnostic(&self) -> Option<needy_api_proto::v1::ErrorDiagnostic> {
    let value = match self {
      Self::Status(status) => crate::diagnostic::from_status(status),
      Self::IdentityMismatch {
        expected_role,
        expected_instance,
        role,
        instance,
      } => Some(needy_api_proto::v1::ErrorDiagnostic {
        reason: if role != expected_role {
          "connection.wrong_role"
        } else {
          "connection.wrong_instance"
        }
        .into(),
        facts: [
          ("expected_role".into(), expected_role.clone()),
          ("expected_instance".into(), expected_instance.clone()),
          ("role".into(), role.clone()),
          ("instance".into(), instance.clone()),
        ]
        .into(),
      }),
      Self::Connector {
        reason,
        program,
        exit_code,
        ..
      } => {
        let mut facts = std::collections::HashMap::from([("program".into(), program.clone())]);
        if let Some(code) = exit_code {
          facts.insert("exit_code".into(), code.to_string());
        }
        Some(needy_api_proto::v1::ErrorDiagnostic {
          reason: (*reason).into(),
          facts,
        })
      }
      Self::RemoteDiagnostic { diagnostic, .. } => needy_api_proto::diagnostic::validated(diagnostic),
      _ => None,
    }?;
    needy_api_proto::diagnostic::validated(&value)
  }

  pub fn kind(&self) -> Kind {
    match self {
      Self::Status(status) => kind(status.code()),
      Self::Transport(_) | Self::Unreachable { .. } | Self::Connector { .. } => Kind::Unavailable,
      Self::InvalidEndpoint(_) | Self::InvalidToken | Self::IdentityMismatch { .. } => Kind::Invalid,
      Self::Failed { .. } => Kind::Failed,
      Self::Ended { .. } => Kind::Ended,
      Self::Remote(_) | Self::RemoteDiagnostic { .. } | Self::Closed => Kind::Other,
    }
  }

  /// Whether retrying the same call may succeed: the server is not up yet.
  pub fn is_transient(&self) -> bool {
    self.kind() == Kind::Unavailable
  }
}

impl From<tonic::Status> for Error {
  fn from(status: tonic::Status) -> Self {
    Self::Status(status)
  }
}

fn kind(code: tonic::Code) -> Kind {
  use tonic::Code;
  match code {
    Code::Unauthenticated => Kind::Unauthenticated,
    Code::PermissionDenied => Kind::Denied,
    Code::NotFound => Kind::NotFound,
    Code::InvalidArgument => Kind::Invalid,
    Code::FailedPrecondition => Kind::Failed,
    Code::ResourceExhausted => Kind::Exhausted,
    Code::Unavailable => Kind::Unavailable,
    _ => Kind::Other,
  }
}

#[cfg(test)]
#[path = "error_test.rs"]
mod tests;
