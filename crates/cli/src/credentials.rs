//! The client tokens this machine holds, by control plane endpoint:
//! `needy pair` stores one, and every command that reaches that endpoint
//! presents it unless `NEEDY_TOKEN` is set. The endpoint paired last is the
//! default for commands that name none (owner decision, 2026-11-12).
//!
//! One JSON file, `credentials.json` in the config directory, private to
//! the user: the directory is created 0700 (one that exists keeps its mode,
//! with a warning when others may enter it) and the file written 0600 to a
//! temporary file that is renamed over the old one, so a reader never sees a
//! half-written file. The file holds tokens in plain text, as `~/.ssh` keys
//! and `~/.kube/config` do; the control plane stores only their hashes.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

/// The file's schema version. A file of another version is an error, not
/// overwritten.
const VERSION: u32 = 1;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
  version: u32,
  /// By endpoint, as [`key`] writes it.
  endpoints: BTreeMap<String, Credential>,
  /// The endpoint paired last, which commands reach when neither
  /// `--endpoint` nor `NEEDY_ENDPOINT` names one. A file written before it
  /// existed has none.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  default_endpoint: Option<String>,
}

/// A client token for one endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credential {
  pub client_id: String,
  /// The client's name on that control plane, as its trace records it.
  pub name: String,
  pub token: String,
  pub paired_at_ms: i64,
}

/// An endpoint as the file keys it: as given, without a trailing `/`, so
/// `http://host:7443/` and `http://host:7443` are one endpoint.
fn key(endpoint: &str) -> String {
  endpoint.trim_end_matches('/').to_string()
}

fn read(path: &Path) -> Result<File> {
  let text = match std::fs::read_to_string(path) {
    Ok(text) => text,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
      return Ok(File {
        version: VERSION,
        ..File::default()
      });
    }
    Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
  };
  let file: File = serde_json::from_str(&text).with_context(|| format!("{} cannot be read", path.display()))?;
  if file.version != VERSION {
    anyhow::bail!("{} is of version {}, not {VERSION}; move it away to pair afresh", path.display(), file.version);
  }
  Ok(file)
}

/// The token stored for `endpoint` in the file at `path`, if any.
pub fn find(path: &Path, endpoint: &str) -> Result<Option<Credential>> {
  Ok(read(path)?.endpoints.remove(&key(endpoint)))
}

/// The endpoint paired last, in the file at `path`, if any.
pub fn default_endpoint(path: &Path) -> Result<Option<String>> {
  Ok(read(path)?.default_endpoint)
}

/// Stores `credential` for `endpoint` in the file at `path`, replacing one
/// stored before, and makes `endpoint` the default. Returns a warning for
/// the user when the file's directory existed already and is open to other
/// users.
pub fn store(path: &Path, endpoint: &str, credential: Credential) -> Result<Option<String>> {
  let mut file = read(path)?;
  file.endpoints.insert(key(endpoint), credential);
  file.default_endpoint = Some(key(endpoint));
  let text = serde_json::to_string_pretty(&file)? + "\n";
  write_private(path, text.as_bytes())
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> Result<Option<String>> {
  use std::io::Write as _;
  use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
  let dir = path.parent().context("the credentials file has no directory")?;
  needy_api_server::create_private_dir(dir).with_context(|| format!("cannot create {}", dir.display()))?;
  // NOTICE: a directory that existed keeps its mode, as the daemon's state
  // and runtime directories do: it may be one the user chose
  // (`NEEDY_CONFIG_DIR`) and shares, such as `/tmp`. The file and its
  // temporary are 0600 either way, so the token stays private; the user is
  // told about the directory instead.
  let mode = std::fs::metadata(dir).with_context(|| format!("cannot read {}", dir.display()))?.permissions().mode() & 0o777;
  let warning = (mode & 0o077 != 0)
    .then(|| format!("{} is open to other users (mode {mode:o}); credentials.json is not; consider chmod 700", dir.display()));
  let temporary = path.with_extension("json.tmp");
  // A temporary file a crash left behind is replaced.
  if let Err(e) = std::fs::remove_file(&temporary)
    && e.kind() != std::io::ErrorKind::NotFound
  {
    return Err(e).with_context(|| format!("cannot remove {}", temporary.display()));
  }
  let mut file = std::fs::OpenOptions::new()
    .write(true)
    .create_new(true)
    .mode(0o600)
    .open(&temporary)
    .with_context(|| format!("cannot create {}", temporary.display()))?;
  file.write_all(bytes).and_then(|()| file.sync_all()).with_context(|| format!("cannot write {}", temporary.display()))?;
  std::fs::rename(&temporary, path).with_context(|| format!("cannot replace {}", path.display()))?;
  Ok(warning)
}

// TODO(windows): the file's ACL is not narrowed on Windows; the CLI's client
// side has not been needed there yet.
#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> Result<Option<String>> {
  let dir = path.parent().context("the credentials file has no directory")?;
  std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
  std::fs::write(path, bytes).with_context(|| format!("cannot write {}", path.display()))?;
  Ok(None)
}

#[cfg(test)]
#[path = "credentials_test.rs"]
mod tests;
