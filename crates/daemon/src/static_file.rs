//! What the hot-reloaded credential files (`--runners-file`,
//! `--clients-file`) share: the stamp that tells a reload whether the file
//! changed, reading a stored SHA-256 digest written as hex, and the length
//! rule for a plain unprefixed secret.

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

/// Refuses a plain unprefixed secret (a runner's `credential`, a client's
/// `token`) that cannot carry 256 bits: it must be at least 64 hex digits,
/// or at least 43 characters of base64 or base64url (padding not counted),
/// the length `openssl rand -hex 32` and `openssl rand -base64 32` print.
/// A value of hex digits only is read as hex. An entry written as its hash
/// cannot be checked this way: the file holds no secret to measure.
///
/// NOTICE: an unprefixed secret is found by its SHA-256 alone, so its
/// entropy is all that keeps it from being guessed; a prefixed one (which
/// needy generates with 32 random bytes) is checked by its kind and ID.
pub(crate) fn check_plain_secret(secret: &str) -> Result<()> {
  const HEX_DIGITS: usize = 64;
  const BASE64_CHARS: usize = 43;
  if secret.bytes().all(|byte| byte.is_ascii_hexdigit()) {
    if secret.len() < HEX_DIGITS {
      bail!("a hex secret needs at least {HEX_DIGITS} digits (32 bytes), got {}", secret.len());
    }
    return Ok(());
  }
  let unpadded = secret.trim_end_matches('=');
  if secret.len() - unpadded.len() > 2 || !unpadded.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"+/-_".contains(&byte)) {
    bail!("a secret must be hex or base64(url) text");
  }
  if unpadded.len() < BASE64_CHARS {
    bail!("a base64 secret needs at least {BASE64_CHARS} characters (32 bytes), got {}", unpadded.len());
  }
  Ok(())
}

#[cfg(test)]
#[path = "static_file_test.rs"]
mod tests;
