//! Hot-reloaded static control-plane clients loaded from a mounted file
//! (`--clients-file`).
//!
//! A static client is provisioned before the daemon starts (a controller
//! writes a Secret and mounts it) instead of pairing. Its token either has
//! the client token's shape, `needy-client.static-NAME.<64 hex digits>`, and
//! is found by its ID (the `static-` ID prefix never matches a paired
//! client's ID, 12 hex digits, so a token names exactly one registry), or is
//! any other high-entropy secret, found by its SHA-256 among the file's
//! entries; paired clients always have prefixed tokens. The file is read again whenever it
//! changes; a removed or rotated entry stops authenticating on the next
//! request. Static clients are listed with paired ones and cannot be
//! revoked through the API: the file is their only source.
//!
//! The file is the authority for a name: while a static client holds a name,
//! no pairing code is created for it and a paired client with that name is
//! refused, so the trace's `client:NAME` keeps naming one principal.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context as _, Result, bail};
use needy_api_server::control::ClientAccess;
use needy_common::sync::lock;
use needy_common::time::now_ms;
use serde::Deserialize;

use crate::static_file::{Stamp, check_plain_secret, parse_sha256_hex};
use crate::store;

/// The ID prefix of every static client: `static-NAME`.
const ID_PREFIX: &str = "static-";

/// On-disk static clients file: `{ clients: [...] }`. Unknown keys are
/// errors at every level.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDocument {
  #[serde(default)]
  clients: Vec<FileEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileEntry {
  name: String,
  #[serde(default)]
  access: FileAccess,
  #[serde(default)]
  token: Option<String>,
  /// SHA-256 of the whole token, as 64 hex digits (optionally `sha256:`).
  #[serde(default)]
  token_hash: Option<String>,
}

/// The grant written in the file, as `pair create --access` names it.
#[derive(Debug, Default, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum FileAccess {
  #[default]
  Runs,
  Admin,
}

impl From<FileAccess> for ClientAccess {
  fn from(access: FileAccess) -> Self {
    match access {
      FileAccess::Runs => ClientAccess::Runs,
      FileAccess::Admin => ClientAccess::Admin,
    }
  }
}

/// Live in-memory state for one statically configured client.
#[derive(Debug, Clone)]
pub(crate) struct StaticClient {
  pub(crate) id: String,
  pub(crate) name: String,
  pub(crate) access: ClientAccess,
  token_hash: Vec<u8>,
  pub(crate) created_ms: i64,
  pub(crate) last_used_ms: Option<i64>,
}

/// What [`StaticClients::authenticate`] finds for a token.
pub(crate) enum Check {
  /// The token names no static client.
  NotStatic,
  /// The token names a static client but is not its token.
  Refused,
  Client(StaticClient),
}

#[derive(Default)]
struct Inner {
  stamp: Option<Stamp>,
  /// By ID (`static-NAME`).
  clients: BTreeMap<String, StaticClient>,
  /// Client IDs by token hash, for unprefixed tokens.
  by_hash: BTreeMap<Vec<u8>, String>,
}

/// Registry of statically configured clients backed by an optional file that
/// reloads whenever its `(mtime, inode, size)` changes.
pub(crate) struct StaticClients {
  path: Option<PathBuf>,
  inner: Mutex<Inner>,
}

impl StaticClients {
  pub(crate) fn new(path: Option<PathBuf>) -> Self {
    Self {
      path,
      inner: Mutex::default(),
    }
  }

  /// Reloads the clients file if it changed on disk. A file that is gone
  /// removes every static client; one that does not parse keeps the previous
  /// entries and is retried on the next reload.
  pub(crate) fn reload(&self) {
    let Some(path) = &self.path else {
      return;
    };
    let meta = match std::fs::metadata(path) {
      Ok(meta) => meta,
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
        let mut inner = lock(&self.inner);
        inner.stamp = None;
        if !inner.clients.is_empty() {
          tracing::info!("clients file {} is absent; static clients removed", path.display());
          inner.clients.clear();
        }
        inner.by_hash.clear();
        return;
      }
      Err(e) => {
        tracing::warn!("cannot stat clients file {}: {e}", path.display());
        return;
      }
    };
    let stamp = Stamp::of(&meta);
    if lock(&self.inner).stamp.as_ref() == Some(&stamp) {
      return;
    }
    // Read and parse outside the mutex so disk I/O never blocks concurrent
    // authentication, and only commit `inner.stamp` on `Ok` so transient
    // errors are retried on the next reload.
    let parsed = match load_file(path) {
      Ok(parsed) => parsed,
      Err(e) => {
        tracing::warn!("failed to reload clients file {}: {e:#}; keeping previous entries", path.display());
        return;
      }
    };
    let mut inner = lock(&self.inner);
    inner.stamp = Some(stamp);
    let now = now_ms();
    let mut next = BTreeMap::new();
    for (id, entry) in parsed {
      let old = inner.clients.get(&id);
      next.insert(
        id.clone(),
        StaticClient {
          id,
          name: entry.name,
          access: entry.access,
          token_hash: entry.token_hash,
          created_ms: old.map_or(now, |old| old.created_ms),
          last_used_ms: old.and_then(|old| old.last_used_ms),
        },
      );
    }
    for removed in inner.clients.keys().filter(|id| !next.contains_key(*id)) {
      tracing::info!("static client {removed} removed from the clients file");
    }
    inner.by_hash = next.values().map(|client| (client.token_hash.clone(), client.id.clone())).collect();
    inner.clients = next;
  }

  /// Whether a static client has the ID `id`.
  pub(crate) fn contains(&self, id: &str) -> bool {
    lock(&self.inner).clients.contains_key(id)
  }

  /// Whether a static client holds the name `name`.
  pub(crate) fn holds_name(&self, name: &str) -> bool {
    lock(&self.inner).clients.values().any(|client| client.name == name)
  }

  /// Checks a prefixed client token against the static clients by the ID it
  /// carries, recording when a matching one was last used.
  pub(crate) fn authenticate(&self, token: &str, now: i64) -> Check {
    let Some(id) = store::secret_id(store::CLIENT_SECRET_KIND, token) else {
      return Check::NotStatic;
    };
    let mut inner = lock(&self.inner);
    let Some(entry) = inner.clients.get_mut(id) else {
      return Check::NotStatic;
    };
    if !store::secret_matches(token, &entry.token_hash) {
      return Check::Refused;
    }
    entry.last_used_ms = Some(now);
    Check::Client(entry.clone())
  }

  /// The static client whose token is the unprefixed secret `token`, found
  /// by its hash, recording when it was last used. A prefixed token is
  /// checked by [`Self::authenticate`] instead.
  ///
  /// NOTICE: the map compares digests in time that depends on where they
  /// differ. That leaks only how a SHA-256 of the caller's own guess relates
  /// to the stored digests; finding a secret from its digest needs a
  /// preimage, and plain secrets carry 256 bits
  /// ([`crate::static_file::check_plain_secret`]).
  pub(crate) fn find(&self, token: &str, now: i64) -> Option<StaticClient> {
    let mut inner = lock(&self.inner);
    let id = inner.by_hash.get(&store::hash_secret(token))?.clone();
    let entry = inner.clients.get_mut(&id)?;
    entry.last_used_ms = Some(now);
    Some(entry.clone())
  }

  /// The name of the static client whose token is the unprefixed secret
  /// `token`, without counting it as a use.
  pub(crate) fn holds(&self, token: &str) -> Option<String> {
    let inner = lock(&self.inner);
    let id = inner.by_hash.get(&store::hash_secret(token))?;
    inner.clients.get(id).map(|client| client.name.clone())
  }

  /// Snapshot of every currently configured static client.
  pub(crate) fn list(&self) -> Vec<StaticClient> {
    lock(&self.inner).clients.values().cloned().collect()
  }
}

struct ParsedEntry {
  name: String,
  access: ClientAccess,
  token_hash: Vec<u8>,
}

/// The entries of the clients file by ID.
fn load_file(path: &Path) -> Result<BTreeMap<String, ParsedEntry>> {
  let text = std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
  if text.trim().is_empty() {
    return Ok(BTreeMap::new());
  }
  let doc: FileDocument = serde_yaml::from_str(&text).with_context(|| format!("invalid YAML in {}", path.display()))?;
  let mut parsed = BTreeMap::new();
  let mut by_hash: BTreeMap<Vec<u8>, String> = BTreeMap::new();
  for entry in doc.clients {
    store::check_client_name(&entry.name).map_err(|e| anyhow::anyhow!("{e}"))?;
    let id = format!("{ID_PREFIX}{}", entry.name);
    if parsed.contains_key(&id) {
      bail!("duplicate static client {:?}", entry.name);
    }
    let token_hash = match (entry.token, entry.token_hash) {
      (Some(token), None) => {
        let token = token.trim();
        match store::secret_id(store::CLIENT_SECRET_KIND, token) {
          Some(named) if named != id => {
            bail!("static client {:?}: a prefixed token must start with `{}.{id}.`", entry.name, store::CLIENT_SECRET_KIND)
          }
          Some(_) => {}
          None if store::is_runner_credential(token) => bail!("static client {:?}: token is a runner credential", entry.name),
          None => check_plain_secret(token).with_context(|| format!("static client {:?}: token is too weak", entry.name))?,
        }
        store::hash_secret(token)
      }
      (None, Some(digest)) => {
        parse_sha256_hex(digest.trim()).with_context(|| format!("static client {:?}: invalid token_hash", entry.name))?
      }
      (Some(_), Some(_)) => bail!("static client {:?}: set only one of token or token_hash", entry.name),
      (None, None) => bail!("static client {:?}: missing token or token_hash", entry.name),
    };
    // An unprefixed token is found by its hash, so two entries with one hash
    // would leave the file to decide which client a token is.
    if let Some(other) = by_hash.insert(token_hash.clone(), entry.name.clone()) {
      bail!("static clients {other:?} and {:?} have the same token", entry.name);
    }
    parsed.insert(
      id,
      ParsedEntry {
        name: entry.name,
        access: entry.access.into(),
        token_hash,
      },
    );
  }
  Ok(parsed)
}

#[cfg(test)]
#[path = "static_clients_test.rs"]
mod tests;
