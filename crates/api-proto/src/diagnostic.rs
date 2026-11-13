//! Validation of the additive diagnostic wire value. Unknown reasons carry
//! no facts: a peer cannot use a new reason to forward arbitrary metadata.

use prost::Message as _;

use crate::v1::ErrorDiagnostic;

/// Metadata is separate from the raw `ExecRefusal` status details.
pub const METADATA_KEY: &str = "needy-diagnostic-v1-bin";

pub fn validated(diagnostic: &ErrorDiagnostic) -> Option<ErrorDiagnostic> {
  if diagnostic.reason.is_empty()
    || diagnostic.encoded_len() > 8192
    || diagnostic.facts.len() > 16
    || diagnostic.reason.len() > 128
    || !diagnostic.reason.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_'))
  {
    return None;
  }
  let keys: &[&str] = match diagnostic.reason.as_str() {
    "process.cwd_invalid" => &["cwd", "program", "executor", "runner", "path_map"],
    "process.start_failed" | "process.terminal_failed" => &["program", "executor", "runner"],
    "auth.backend_unavailable" => &[],
    "executor.authentication_failed" => &["executor", "runner"],
    "runner.not_found"
    | "runner.not_connected"
    | "runner.offline"
    | "runner.capability_missing"
    | "runner.selector_no_match"
    | "runner.selector_ambiguous" => &["runner", "need", "selector", "candidates", "driver"],
    "connection.wrong_role" | "connection.wrong_instance" => &["role", "instance", "expected_role", "expected_instance"],
    "connector.not_found" | "connector.start_failed" | "connector.failed" => &["program", "exit_code"],
    "auth.admin_required" | "auth.role_denied" => &["caller", "access"],
    "bootstrap.invalid_code" | "bootstrap.expired" | "bootstrap.used" | "bootstrap.revoked" | "bootstrap.scope_denied" => &["purpose", "id"],
    "client.invitation_open" | "client.not_found" => &["client"],
    "runner.name_in_use" => &["runner"],
    "run.not_found" | "run.ambiguous" | "run.ended" | "run.no_terminal" | "run.no_console" => &["run", "state", "candidates"],
    "process.not_found" | "process.internal_attach_denied" => &["process", "origin"],
    "state.read_failed" | "state.write_failed" => &["store"],
    _ => &[],
  };
  if diagnostic.facts.values().any(|value| value.len() > 1024 || value.chars().any(char::is_control)) {
    return None;
  }
  Some(ErrorDiagnostic {
    reason: diagnostic.reason.clone(),
    facts: diagnostic
      .facts
      .iter()
      .filter(|(key, _)| keys.contains(&key.as_str()))
      .map(|(key, value)| (key.clone(), value.clone()))
      .collect(),
  })
}
