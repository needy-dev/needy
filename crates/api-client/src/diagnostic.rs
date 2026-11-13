//! Known diagnostic metadata and runner error conversions. Arbitrary peer
//! metadata is never copied across the runner bridge.

use needy_api_proto::{
  diagnostic,
  v1::{ErrorDiagnostic, ExecRefusal, TaskError},
};
use prost::Message as _;
use tonic::{Code, Status};

pub fn from_status(status: &Status) -> Option<ErrorDiagnostic> {
  let bytes = status.metadata().get_bin(diagnostic::METADATA_KEY)?.to_bytes().ok()?;
  if bytes.len() > 8192 {
    return None;
  }
  diagnostic::validated(&ErrorDiagnostic::decode(bytes).ok()?)
}

pub fn with_status(mut status: Status, value: ErrorDiagnostic) -> Status {
  if let Some(value) = diagnostic::validated(&value) {
    status.metadata_mut().insert_bin(diagnostic::METADATA_KEY, tonic::metadata::MetadataValue::from_bytes(&value.encode_to_vec()));
  }
  status
}

pub fn task_error(status: &Status) -> TaskError {
  TaskError {
    code: status.code() as i32,
    message: status.message().to_string(),
    details: if status.code() == Code::PermissionDenied && ExecRefusal::decode(status.details()).is_ok() {
      status.details().to_vec()
    } else {
      Vec::new()
    },
    diagnostic: from_status(status),
  }
}

pub fn task_status(error: TaskError) -> Status {
  let code = Code::from_i32(error.code);
  let details = if code == Code::PermissionDenied && ExecRefusal::decode(error.details.as_slice()).is_ok() {
    error.details
  } else {
    Vec::new()
  };
  let status = Status::with_details(code, error.message, details.into());
  match error.diagnostic {
    Some(value) => with_status(status, value),
    None => status,
  }
}
