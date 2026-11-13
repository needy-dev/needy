//! The one table from [`ControlError`] to a gRPC status. Clients map the
//! codes back (`needy_api_client::Kind`).
//!
//! | Error | Code | Details |
//! | --- | --- | --- |
//! | `Denied` | `PERMISSION_DENIED` | `ExecRefusal` with the refused command's events |
//! | `Unauthenticated` | `UNAUTHENTICATED` | |
//! | `NotFound` | `NOT_FOUND` | |
//! | `Invalid` | `INVALID_ARGUMENT` | |
//! | `Failed` | `FAILED_PRECONDITION` | |
//! | `Exhausted` | `RESOURCE_EXHAUSTED` | |
//! | `Unavailable` | `UNAVAILABLE` | |
//! | `Internal` | `INTERNAL` | |

use needy_api_proto::v1::ExecRefusal;
use prost::Message as _;
use tonic::{Code, Status};

use super::domain;
use crate::control::ControlError;

pub fn status(error: ControlError) -> Status {
  match error {
    ControlError::Diagnostic { error, diagnostic } => {
      let mut status = status(*error);
      if let Some(diagnostic) = domain::proto_diagnostic(diagnostic) {
        status
          .metadata_mut()
          .insert_bin(needy_api_proto::diagnostic::METADATA_KEY, tonic::metadata::MetadataValue::from_bytes(&diagnostic.encode_to_vec()));
      }
      status
    }

    ControlError::Denied { message, events } => {
      let details = ExecRefusal {
        events: events.iter().map(domain::proto_exec_event).collect(),
      };
      Status::with_details(Code::PermissionDenied, message, details.encode_to_vec().into())
    }
    ControlError::Unauthenticated(message) => Status::unauthenticated(message),
    ControlError::NotFound(message) => Status::not_found(message),
    ControlError::Invalid(message) => Status::invalid_argument(message),
    ControlError::Failed(message) => Status::failed_precondition(message),
    ControlError::Exhausted(message) => Status::resource_exhausted(message),
    ControlError::Unavailable(message) => Status::unavailable(message),
    ControlError::Internal(message) => Status::internal(message),
  }
}

impl From<ControlError> for Status {
  fn from(error: ControlError) -> Self {
    status(error)
  }
}

impl From<domain::Error> for ControlError {
  /// A request field this build cannot read.
  fn from(error: domain::Error) -> Self {
    ControlError::Invalid(error.to_string())
  }
}

#[cfg(test)]
#[path = "status_test.rs"]
mod tests;
