//! What the hot-reloaded credential files (`--runners-file`,
//! `--clients-file`) share: the stamp that tells a reload whether the file
//! changed, and reading a stored SHA-256 digest written as hex.

use std::os::unix::fs::MetadataExt as _;
use std::time::SystemTime;

use anyhow::{Context as _, Result, bail};

/// A file's `(mtime, inode, size)`. A mounted Kubernetes Secret is replaced
/// by an atomic symlink swap, which changes the inode even when the mtime
/// resolution hides the write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Stamp {
  modified: Option<SystemTime>,
  ino: u64,
  len: u64,
}

impl Stamp {
  pub(crate) fn of(meta: &std::fs::Metadata) -> Self {
    Self {
      modified: meta.modified().ok(),
      ino: meta.ino(),
      len: meta.len(),
    }
  }
}

/// The 32 bytes of a SHA-256 digest written as 64 hex digits, optionally
/// prefixed `sha256:`.
pub(crate) fn parse_sha256_hex(text: &str) -> Result<Vec<u8>> {
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
