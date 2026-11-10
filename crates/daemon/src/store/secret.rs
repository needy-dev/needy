//! Join tokens and runner credentials: generating them, and checking one
//! against its stored hash.
//!
//! A secret is `<kind>.<id>.<64 hex digits>`: 32 random bytes after a prefix
//! that names its kind and the row it belongs to (a join token's ID or a
//! runner's name), so a check reads one row and compares one hash. Only the
//! SHA-256 of the whole string is stored. A salted, slow hash is not needed:
//! the secret has 256 bits of entropy, so the hash cannot be guessed back.

use rand::RngCore as _;
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;

/// The prefix of a join token.
pub const JOIN: &str = "needy-join";
/// The prefix of a runner credential.
pub const RUNNER: &str = "needy-runner";

/// A secret in plain text, returned once to whoever it is for. Its `Debug`
/// form does not show it.
pub struct Secret(String);

impl Secret {
  /// A new secret of `kind` for the row `id`.
  pub fn generate(kind: &str, id: &str) -> Self {
    Self(format!("{kind}.{id}.{}", hex(&random::<32>())))
  }

  #[cfg(test)]
  pub fn expose(&self) -> &str {
    &self.0
  }

  pub fn into_string(self) -> String {
    self.0
  }

  pub fn hash(&self) -> Vec<u8> {
    hash(&self.0)
  }
}

impl std::fmt::Debug for Secret {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("Secret(..)")
  }
}

/// The row `secret` names when it is of `kind`.
pub fn id<'a>(kind: &str, secret: &'a str) -> Option<&'a str> {
  let mut parts = secret.splitn(3, '.');
  let (prefix, id, _) = (parts.next()?, parts.next()?, parts.next()?);
  (prefix == kind && !id.is_empty()).then_some(id)
}

pub fn hash(secret: &str) -> Vec<u8> {
  Sha256::digest(secret.as_bytes()).to_vec()
}

/// Whether `secret` hashes to `stored`, in time that does not depend on
/// where they differ.
pub fn matches(secret: &str, stored: &[u8]) -> bool {
  bool::from(hash(secret).ct_eq(stored))
}

/// A short random ID, such as a join token's.
pub fn random_id() -> String {
  hex(&random::<6>())
}

fn random<const N: usize>() -> [u8; N] {
  let mut bytes = [0; N];
  rand::rng().fill_bytes(&mut bytes);
  bytes
}

fn hex(bytes: &[u8]) -> String {
  use std::fmt::Write as _;
  bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut text, byte| {
    // Writing to a `String` cannot fail.
    let _ = write!(text, "{byte:02x}");
    text
  })
}
