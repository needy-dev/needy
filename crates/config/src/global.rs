//! The versioned client document and daemon defaults. Validation is pure:
//! resolving paths, opening transports and writing private files belong to callers.

use crate::value::Duration;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// The sole supported global configuration schema.
pub const VERSION: u32 = 1;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
  pub version: u32,
  pub current_context: String,
  pub contexts: BTreeMap<String, Context>,
  pub paths: Paths,
  pub daemon: Daemon,
}

impl Default for Config {
  fn default() -> Self {
    Self {
      version: VERSION,
      current_context: "local".into(),
      contexts: BTreeMap::new(),
      paths: Paths::default(),
      daemon: Daemon::default(),
    }
  }
}

/// A saved transport and its independent, server-issued identity.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub address: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub command: Option<Vec<String>>,
  #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
  pub headers: BTreeMap<String, String>,
  pub identity: Identity,
  pub credential: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
  pub client_id: String,
  pub name: String,
  /// A recorded grant, never a live permission assertion.
  pub access: Access,
  pub paired_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
  Runs,
  Admin,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Paths {
  pub state: Option<PathBuf>,
  pub data: Option<PathBuf>,
  pub runtime: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Daemon {
  pub listen: Vec<String>,
  pub actor_image: Option<PathBuf>,
  pub local_runner: Option<String>,
  pub guest_needy: Option<PathBuf>,
  pub executor_image: Option<String>,
  pub heartbeat_interval: Duration,
  pub engine_host: Option<String>,
  pub process_path: Option<Vec<PathBuf>>,
  pub disable_host_files: bool,
  pub host_files_root: Option<PathBuf>,
  pub gateway_blocked_cidrs: Vec<String>,
}

impl Default for Daemon {
  fn default() -> Self {
    Self {
      listen: Vec::new(),
      actor_image: None,
      local_runner: None,
      guest_needy: None,
      executor_image: None,
      heartbeat_interval: Duration::from_secs(15),
      engine_host: None,
      process_path: None,
      disable_host_files: false,
      host_files_root: None,
      gateway_blocked_cidrs: Vec::new(),
    }
  }
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct Error {
  message: String,
  field: String,
  version: bool,
}
impl Error {
  fn invalid(field: impl Into<String>, message: impl Into<String>) -> Self {
    Self {
      message: message.into(),
      field: field.into(),
      version: false,
    }
  }
  pub fn reason(&self) -> &'static str {
    if self.version {
      "config.version_unsupported"
    } else {
      "config.invalid_field"
    }
  }
  pub fn field(&self) -> &str {
    &self.field
  }
}

/// DNS label names shared by saved contexts and registered runner names.
pub fn valid_name(name: &str) -> bool {
  !name.is_empty()
    && name.len() <= 63
    && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    && name.as_bytes()[0].is_ascii_alphanumeric()
    && name.as_bytes()[name.len() - 1].is_ascii_alphanumeric()
}

/// Validate a direct transport without contacting it.
pub fn validate_address(address: &str, listener: bool) -> Result<(), Error> {
  if let Some(path) = address.strip_prefix("unix:") {
    if !std::path::Path::new(path).is_absolute() {
      return Err(Error::invalid("address", "unix address requires an absolute path"));
    }
    return Ok(());
  }
  let uri: http::Uri = address.parse().map_err(|_| {
    Error::invalid("address", "address must be unix:///absolute/path, tcp://host:port, http://host:port or https://host:port")
  })?;
  let scheme = uri.scheme_str().unwrap_or_default();
  if !matches!(scheme, "tcp" | "http" | "https")
    || (listener && scheme != "tcp")
    || uri.host().is_none()
    || uri.authority().is_none_or(|authority| authority.as_str().contains('@'))
    || (scheme == "tcp" && uri.port_u16().is_none())
    || uri.path_and_query().is_some_and(|p| p.as_str() != "/")
  {
    return Err(Error::invalid("address", "invalid address scheme, host, port, userinfo or path"));
  }
  Ok(())
}

/// Validate static ASCII request headers attached to a saved context.
pub fn validate_headers(headers: &BTreeMap<String, String>) -> Result<(), Error> {
  let mut seen = BTreeSet::new();
  for (name, value) in headers {
    let field = format!("headers.{name}");
    let lower = name.to_ascii_lowercase();
    if lower.is_empty()
      || lower.ends_with("-bin")
      || !lower.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.'))
    {
      return Err(Error::invalid(
        field,
        "header name must be a nonempty ASCII gRPC metadata key (letters, digits, '-', '_', '.'; not '-bin')",
      ));
    }
    if lower == "authorization" {
      return Err(Error::invalid(field, "authorization header is reserved for the context credential"));
    }
    if !seen.insert(lower) {
      return Err(Error::invalid(field, "duplicate header name (header names are case-insensitive)"));
    }
    if value.is_empty() || !value.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
      return Err(Error::invalid(field, "header value must be nonempty printable ASCII"));
    }
  }
  Ok(())
}

impl Context {
  pub fn validate(&self) -> Result<(), Error> {
    match (&self.address, &self.command) {
      (Some(address), None) => validate_address(address, false)?,
      (None, Some(command)) if !command.is_empty() && command.iter().all(|s| !s.contains('\0')) && !command[0].is_empty() => {}
      _ => return Err(Error::invalid("transport", "a context requires exactly one address or nonempty connector argv")),
    }
    validate_headers(&self.headers)?;
    if self.identity.client_id.is_empty() {
      return Err(Error::invalid("identity.client_id", "a context requires a nonempty client ID"));
    }
    if !valid_name(&self.identity.name) {
      return Err(Error::invalid("identity.name", "a context requires a valid client name"));
    }
    if self.credential.is_empty() {
      return Err(Error::invalid("credential", "a context requires a nonempty credential"));
    }
    Ok(())
  }
}

impl Config {
  /// Reject unsupported versions and invalid complete candidates before a write.
  pub fn validate(&self) -> Result<(), Error> {
    if self.version != VERSION {
      return Err(Error {
        message: format!("unsupported configuration version {}; expected {VERSION}", self.version),
        field: "version".into(),
        version: true,
      });
    }
    if self.current_context != "local" && !self.contexts.contains_key(&self.current_context) {
      return Err(Error::invalid("current_context", "current_context does not name an existing context"));
    }
    for (name, context) in &self.contexts {
      if name == "local" || !valid_name(name) {
        return Err(Error::invalid(format!("contexts.{name}"), "invalid context name; local is reserved, use a DNS label"));
      }
      context.validate().map_err(|mut error| {
        error.field = format!("contexts.{name}.{}", error.field);
        error
      })?;
    }
    for (name, path) in [
      ("paths.state", &self.paths.state),
      ("paths.data", &self.paths.data),
      ("paths.runtime", &self.paths.runtime),
    ] {
      if path.as_ref().is_some_and(|p| !p.is_absolute()) {
        return Err(Error::invalid(name, format!("{name} requires an absolute path")));
      }
    }
    let mut seen = BTreeSet::new();
    for address in &self.daemon.listen {
      validate_address(address, true).map_err(|mut error| {
        error.field = "daemon.listen".into();
        error
      })?;
      if !seen.insert(address.trim_end_matches('/')) {
        return Err(Error::invalid("daemon.listen", "daemon.listen contains a duplicate address"));
      }
    }
    if self.daemon.heartbeat_interval.is_zero() {
      return Err(Error::invalid("daemon.heartbeat_interval", "daemon.heartbeat_interval must be positive"));
    }
    if self.daemon.local_runner.as_deref().is_some_and(|s| !valid_name(s)) {
      return Err(Error::invalid("daemon.local_runner", "daemon.local_runner requires a DNS label"));
    }
    if self.daemon.process_path.as_ref().is_some_and(|paths| paths.is_empty() || paths.iter().any(|p| !p.is_absolute())) {
      return Err(Error::invalid("daemon.process_path", "daemon.process_path requires nonempty absolute path entries"));
    }
    if self.daemon.host_files_root.as_ref().is_some_and(|p| !p.is_absolute()) {
      return Err(Error::invalid("daemon.host_files_root", "daemon.host_files_root requires an absolute path"));
    }
    for cidr in &self.daemon.gateway_blocked_cidrs {
      validate_cidr(cidr)?;
    }
    for (name, value) in [
      ("daemon.executor_image", &self.daemon.executor_image),
      ("daemon.engine_host", &self.daemon.engine_host),
    ] {
      if value.as_ref().is_some_and(|v| v.is_empty() || v.contains('\0')) {
        return Err(Error::invalid(name, format!("{name} cannot be empty or contain NUL")));
      }
    }
    Ok(())
  }
}

/// Validate an IPv4 or IPv6 CIDR string (`<ip>/<prefix>`).
pub fn validate_cidr(cidr: &str) -> Result<(), Error> {
  let Some((addr, prefix)) = cidr.split_once('/') else {
    return Err(Error::invalid("daemon.gateway_blocked_cidrs", format!("invalid CIDR {cidr:?}: missing '/'")));
  };
  let ip: std::net::IpAddr =
    addr.parse().map_err(|e| Error::invalid("daemon.gateway_blocked_cidrs", format!("invalid CIDR address {addr:?}: {e}")))?;
  let prefix_len: u8 =
    prefix.parse().map_err(|e| Error::invalid("daemon.gateway_blocked_cidrs", format!("invalid CIDR prefix {prefix:?}: {e}")))?;
  let max = match ip {
    std::net::IpAddr::V4(_) => 32,
    std::net::IpAddr::V6(_) => 128,
  };
  if prefix_len > max {
    return Err(Error::invalid("daemon.gateway_blocked_cidrs", format!("invalid CIDR {cidr:?}: prefix length {prefix_len} exceeds {max}")));
  }
  Ok(())
}

#[cfg(test)]
#[path = "global_test.rs"]
mod tests;
