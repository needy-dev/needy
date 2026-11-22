//! SHA-256 content digests in OCI format (`sha256:<64 lowercase hex digits>`).

use std::fmt;
use std::str::FromStr;

use sha2::{Digest as _, Sha256};

/// A SHA-256 content digest written as OCI writes one, `sha256:<hex>`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest([u8; 32]);

/// Text that is not a `sha256:` digest of 64 lowercase hex digits.
#[derive(Debug, thiserror::Error)]
#[error("{0} is not a digest: expected sha256: and 64 lowercase hex digits")]
pub struct InvalidDigest(String);

impl Digest {
  /// Reads `sha256:<64 lowercase hex digits>`.
  // Reference: https://github.com/opencontainers/image-spec/blob/v1.1.1/descriptor.md#digests
  pub fn parse(text: &str) -> Result<Self, InvalidDigest> {
    let hex = text.strip_prefix("sha256:").ok_or_else(|| InvalidDigest(text.into()))?;
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
      return Err(InvalidDigest(text.into()));
    }
    let mut bytes = [0u8; 32];
    let (pairs, _) = hex.as_bytes().as_chunks::<2>();
    for (byte, &[hi, lo]) in bytes.iter_mut().zip(pairs) {
      *byte = (hex_nibble(hi) << 4) | hex_nibble(lo);
    }
    Ok(Self(bytes))
  }

  /// The SHA-256 digest of `content`.
  pub fn of(content: &[u8]) -> Self {
    Self(Sha256::digest(content).into())
  }

  /// Constructs a digest directly from its 32 raw SHA-256 bytes.
  pub fn from_bytes(bytes: [u8; 32]) -> Self {
    Self(bytes)
  }

  /// The 32 raw SHA-256 bytes.
  pub fn as_bytes(&self) -> &[u8; 32] {
    &self.0
  }

  /// The 64 lowercase hex digits, without the `sha256:` prefix.
  pub fn hex(&self) -> String {
    use std::fmt::Write as _;
    self.0.iter().fold(String::with_capacity(64), |mut hex, byte| {
      // NOTICE: writing to a `String` cannot fail.
      let _ = write!(hex, "{byte:02x}");
      hex
    })
  }
}

fn hex_nibble(byte: u8) -> u8 {
  match byte {
    b'0'..=b'9' => byte - b'0',
    b'a'..=b'f' => byte - b'a' + 10,
    _ => unreachable!("validated lowercase hex above"),
  }
}

impl fmt::Display for Digest {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "sha256:{}", self.hex())
  }
}

impl fmt::Debug for Digest {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    fmt::Display::fmt(self, f)
  }
}

impl FromStr for Digest {
  type Err = InvalidDigest;

  fn from_str(text: &str) -> Result<Self, Self::Err> {
    Self::parse(text)
  }
}

impl serde::Serialize for Digest {
  fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(self)
  }
}

impl<'de> serde::Deserialize<'de> for Digest {
  fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    let text = String::deserialize(deserializer)?;
    Self::parse(&text).map_err(serde::de::Error::custom)
  }
}
