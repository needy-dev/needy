//! Hot-reloaded static runner credentials loaded from a mounted file
//! (`--runners-file`).
//!
//! Static runners live alongside `state.db` runners in authentication,
//! heartbeats, `ListRunners`, and scheduling. Removing a runner from the file
//! (or changing its credential) closes its active control stream immediately on
//! the next reload; `rotate` and `remove` RPCs are refused for static runners.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use anyhow::{Context as _, Result, bail};
use needy_common::sync::lock;
use needy_common::time::now_ms;
use needy_core::types::{Capabilities, RunnerRole};
use serde::Deserialize;

use crate::link::Links;
use crate::store;

/// On-disk static runners file: either `{ runners: [...] }` or a top-level list.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum FileDocument {
  Wrapped { runners: Vec<FileEntry> },
  List(Vec<FileEntry>),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileEntry {
  name: String,
  #[serde(default)]
  credential: Option<String>,
  #[serde(default)]
  credential_hash: Option<String>,
  #[serde(default)]
  roles: BTreeSet<RunnerRole>,
  #[serde(default)]
  labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileStamp {
  modified: Option<SystemTime>,
  ino: u64,
  len: u64,
}

impl FileStamp {
  fn of(meta: &std::fs::Metadata) -> Self {
    Self {
      modified: meta.modified().ok(),
      ino: meta.ino(),
      len: meta.len(),
    }
  }
}

/// Live in-memory state for one statically configured runner.
#[derive(Debug, Clone)]
pub(crate) struct StaticRunner {
  pub(crate) name: String,
  credential_hash: Vec<u8>,
  scope: store::Scope,
  pub(crate) capabilities: Capabilities,
  pub(crate) labels: BTreeMap<String, String>,
  pub(crate) created_ms: i64,
  pub(crate) last_seen_ms: i64,
}

#[derive(Default)]
struct Inner {
  stamp: Option<FileStamp>,
  runners: BTreeMap<String, StaticRunner>,
}

/// Registry of statically configured runners backed by an optional file that
/// reloads whenever its `(mtime, inode, size)` changes.
pub(crate) struct StaticRunners {
  path: Option<PathBuf>,
  inner: Mutex<Inner>,
}

impl StaticRunners {
  pub(crate) fn new(path: Option<PathBuf>) -> Self {
    Self {
      path,
      inner: Mutex::default(),
    }
  }

  /// Checks whether the configured runners file changed on disk and reloads it
  /// if so, closing active control streams of any removed or credential-changed
  /// static runners.
  pub(crate) fn reload(&self, links: &Links) {
    let Some(path) = &self.path else {
      return;
    };
    let meta = match std::fs::metadata(path) {
      Ok(meta) => Some(meta),
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
      Err(e) => {
        tracing::warn!("cannot stat runners file {}: {e}", path.display());
        return;
      }
    };
    let Some(meta) = meta else {
      let removed = {
        let mut inner = lock(&self.inner);
        inner.stamp = None;
        if inner.runners.is_empty() {
          return;
        }
        let removed: Vec<String> = inner.runners.keys().cloned().collect();
        inner.runners.clear();
        removed
      };
      for name in removed {
        tracing::info!("static runner {name} removed (runners file absent); closing control stream");
        links.close(&name);
      }
      return;
    };
    let stamp = FileStamp::of(&meta);
    if lock(&self.inner).stamp.as_ref() == Some(&stamp) {
      return;
    }
    // Read and parse outside the mutex so disk I/O never blocks concurrent auth
    // or heartbeats, and only commit `inner.stamp` on `Ok` so transient errors
    // are retried on the next reload.
    let parsed = match load_file(path) {
      Ok(parsed) => parsed,
      Err(e) => {
        tracing::warn!("failed to reload runners file {}: {e:#}; keeping previous entries", path.display());
        return;
      }
    };
    let to_close = {
      let mut inner = lock(&self.inner);
      inner.stamp = Some(stamp);
      let now = now_ms();
      let mut next = BTreeMap::new();
      let mut close = Vec::new();
      for (name, old) in &inner.runners {
        match parsed.get(name) {
          None => close.push(name.clone()),
          Some(entry) if entry.credential_hash != old.credential_hash => close.push(name.clone()),
          Some(_) => {}
        }
      }
      for (name, entry) in parsed {
        let state = match inner.runners.get(&name) {
          Some(old) => {
            let mut labels = old.labels.clone();
            for (k, v) in &entry.scope.labels {
              labels.insert(k.clone(), v.clone());
            }
            StaticRunner {
              name: entry.name,
              credential_hash: entry.credential_hash,
              scope: entry.scope,
              capabilities: old.capabilities.clone(),
              labels,
              created_ms: old.created_ms,
              last_seen_ms: old.last_seen_ms,
            }
          }
          None => StaticRunner {
            labels: entry.scope.labels.clone(),
            name: entry.name,
            credential_hash: entry.credential_hash,
            scope: entry.scope,
            capabilities: Capabilities::default(),
            created_ms: now,
            last_seen_ms: 0,
          },
        };
        next.insert(name, state);
      }
      inner.runners = next;
      close
    };
    for name in to_close {
      tracing::info!("static runner {name} removed or rotated in runners file; closing control stream");
      links.close(&name);
    }
  }

  /// Whether `name` is currently defined in the static runners file.
  pub(crate) fn contains(&self, name: &str) -> bool {
    lock(&self.inner).runners.contains_key(name)
  }

  /// Authenticates a bearer token against statically configured runners.
  /// Returns `Some(true)` if matched, `Some(false)` if the runner name is
  /// statically configured but the secret did not match, or `None` if the
  /// runner is not statically configured.
  pub(crate) fn authenticate(&self, token: &str) -> Option<(String, bool)> {
    let name = store::secret_id(store::RUNNER_SECRET_KIND, token)?;
    let inner = lock(&self.inner);
    let entry = inner.runners.get(name)?;
    Some((entry.name.clone(), store::secret_matches(token, &entry.credential_hash)))
  }

  /// Records a heartbeat for a static runner, validating against its scope.
  /// Returns `Ok(true)` if handled as a static runner, `Ok(false)` if not static.
  pub(crate) fn heartbeat(&self, name: &str, report: store::Report, now: i64) -> store::Result<bool> {
    let mut inner = lock(&self.inner);
    let Some(entry) = inner.runners.get_mut(name) else {
      return Ok(false);
    };
    let (capabilities, labels) = store::admit(&entry.scope, report)?;
    entry.capabilities = capabilities;
    entry.labels = labels;
    entry.last_seen_ms = now;
    Ok(true)
  }

  /// Marks a static runner as seen at `now`. Returns `true` if static.
  pub(crate) fn seen(&self, name: &str, now: i64) -> bool {
    let mut inner = lock(&self.inner);
    let Some(entry) = inner.runners.get_mut(name) else {
      return false;
    };
    entry.last_seen_ms = now;
    true
  }

  /// Snapshot of all currently configured static runners.
  pub(crate) fn list(&self) -> Vec<StaticRunner> {
    lock(&self.inner).runners.values().cloned().collect()
  }

  /// Snapshot of one statically configured runner.
  pub(crate) fn get(&self, name: &str) -> Option<StaticRunner> {
    lock(&self.inner).runners.get(name).cloned()
  }
}

struct ParsedEntry {
  name: String,
  credential_hash: Vec<u8>,
  scope: store::Scope,
}

fn load_file(path: &Path) -> Result<BTreeMap<String, ParsedEntry>> {
  let text = std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
  if text.trim().is_empty() {
    return Ok(BTreeMap::new());
  }
  let doc: FileDocument = serde_yaml::from_str(&text).with_context(|| format!("invalid YAML in {}", path.display()))?;
  let entries = match doc {
    FileDocument::Wrapped { runners } => runners,
    FileDocument::List(runners) => runners,
  };
  let mut parsed = BTreeMap::new();
  for entry in entries {
    store::check_name(&entry.name).map_err(|e| anyhow::anyhow!("{e}"))?;
    store::check_labels(&entry.labels).map_err(|e| anyhow::anyhow!("{e}"))?;
    if parsed.contains_key(&entry.name) {
      bail!("duplicate static runner {:?}", entry.name);
    }
    let credential_hash = match (entry.credential, entry.credential_hash) {
      (Some(secret), None) => {
        let secret = secret.trim();
        let Some(id) = store::secret_id(store::RUNNER_SECRET_KIND, secret) else {
          bail!("static runner {:?}: credential must start with `{}.{}.`", entry.name, store::RUNNER_SECRET_KIND, entry.name);
        };
        if id != entry.name {
          bail!("static runner {:?}: credential names runner {id:?} instead", entry.name);
        }
        store::hash_secret(secret)
      }
      (None, Some(digest)) => {
        parse_sha256_hex(digest.trim()).with_context(|| format!("static runner {:?}: invalid credential_hash", entry.name))?
      }
      (Some(_), Some(_)) => bail!("static runner {:?}: set only one of credential or credential_hash", entry.name),
      (None, None) => bail!("static runner {:?}: missing credential or credential_hash", entry.name),
    };
    parsed.insert(
      entry.name.clone(),
      ParsedEntry {
        name: entry.name,
        credential_hash,
        scope: store::Scope {
          roles: entry.roles,
          labels: entry.labels,
        },
      },
    );
  }
  Ok(parsed)
}

fn parse_sha256_hex(text: &str) -> Result<Vec<u8>> {
  let hex = text.strip_prefix("sha256:").unwrap_or(text);
  if hex.len() != 64 {
    bail!("expected 64 hex characters, got {}", hex.len());
  }
  let mut bytes = Vec::with_capacity(32);
  for chunk in hex.as_bytes().as_chunks::<2>().0 {
    let pair = std::str::from_utf8(chunk).context("non-UTF-8 hex")?;
    let byte = u8::from_str_radix(pair, 16).with_context(|| format!("invalid hex byte {pair:?}"))?;
    bytes.push(byte);
  }
  Ok(bytes)
}
