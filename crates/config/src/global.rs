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

impl Context {
  pub fn validate(&self) -> Result<(), Error> {
    match (&self.address, &self.command) {
      (Some(address), None) => validate_address(address, false)?,
      (None, Some(command)) if !command.is_empty() && command.iter().all(|s| !s.contains('\0')) && !command[0].is_empty() => {}
      _ => return Err(Error::invalid("transport", "a context requires exactly one address or nonempty connector argv")),
    }
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
