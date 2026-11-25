//! Standalone pure-Rust OCI layer manipulation:
//!
//! - [`Digest`]: SHA-256 content digests in OCI format (`sha256:<64 lowercase hex>`).
//! - [`archive`]: canonical OCI layer tar archives and overlayfs upperdir
//!   whiteout (`char 0/0`, `trusted.overlay.opaque` / `user.overlay.opaque`)
//!   ↔ OCI (`.wh.` / `.wh..wh..opq`) canonicalization.
//! - [`collapse`]: algebraic multi-layer collapse preserving whiteouts,
//!   opaque directories, and broken hard links.
//! - [`oci`]: generic OCI Image Layout (`oci-layout`, `index.json`,
//!   `blobs/sha256/...`) and Docker `tarexport` (`manifest.json`) reading and
//!   hybrid archive writing with caller-supplied post-layer and config hooks.
//! - [`disk`] (behind the `squashfs` feature): read-only SquashFS layer disks
//!   with opaque-directory whiteout expansion for overlayfs mounting.
//!
//! This crate has zero `needy-*` dependencies so it can be reused directly in
//! standalone microVM tools and daemonless Nix pipelines.

// TODO(oci-layer): Tier-2 ecosystem crates (`ocidir`, `ocirender`) were
// evaluated and intentionally deferred for now while `oci-layer` handles
// overlayfs upperdir whiteout conversion (`char 0/0` + PAX
// `trusted.overlay.opaque`) and hybrid OCI + Docker `tarexport` assembly.
// Revisit adopting them once they mature and support these workflows without
// regressions.

pub mod archive;
pub mod collapse;
#[cfg(feature = "squashfs")]
pub mod disk;
pub mod oci;

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
  pub fn parse(text: &str) -> std::result::Result<Self, InvalidDigest> {
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

  /// Converts to an [`oci_spec::image::Digest`].
  pub fn to_oci_digest(&self) -> oci_spec::image::Digest {
    self.to_string().parse().expect("validated sha256 digest parses as oci_spec::image::Digest")
  }
}

/// The ChainID of the layers whose DiffIDs are `diff_ids`, lowest first: one
/// identifier for the root file system they make when applied in order.
/// `None` for no layers.
// Source: https://github.com/opencontainers/image-spec/blob/e7f7c0ca69b21688c3cea7c87a04e4503e6099e2/config.md?plain=1#L42-L53
// (image-spec v1.1.0, "Layer ChainID": `ChainID(L0) = DiffID(L0)`,
// `ChainID(L0|...|Ln) = Digest(ChainID(L0|...|Ln-1) + " " + DiffID(Ln))`)
// Reference: https://github.com/opencontainers/image-spec/blob/v1.1.0/config.md#layer-chainid
// NOTICE: written here because `oci-spec` has the configuration's types but
// does not compute ChainIDs; the definition is the two lines above.
pub fn chain_id(diff_ids: &[Digest]) -> Option<Digest> {
  let (first, rest) = diff_ids.split_first()?;
  Some(rest.iter().fold(*first, |chain, diff_id| Digest::of(format!("{chain} {diff_id}").as_bytes())))
}

fn hex_nibble(byte: u8) -> u8 {
  match byte {
    b'0'..=b'9' => byte - b'0',
    b'a'..=b'f' => byte - b'a' + 10,
    _ => unreachable!("validated lowercase hex above"),
  }
}

impl AsRef<[u8]> for Digest {
  fn as_ref(&self) -> &[u8] {
    &self.0
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

  fn from_str(text: &str) -> std::result::Result<Self, Self::Err> {
    Self::parse(text)
  }
}

impl From<Digest> for oci_spec::image::Digest {
  fn from(digest: Digest) -> Self {
    digest.to_oci_digest()
  }
}

impl serde::Serialize for Digest {
  fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
    serializer.collect_str(self)
  }
}

impl<'de> serde::Deserialize<'de> for Digest {
  fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
    let text = String::deserialize(deserializer)?;
    Self::parse(&text).map_err(serde::de::Error::custom)
  }
}

/// Errors returned by OCI layer operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
  /// A layer archive, image layout, or disk input is malformed or unsupported.
  #[error("invalid OCI layer or image: {0}")]
  Invalid(String),
  /// A filesystem or stream I/O error.
  #[error("{context}: {source}")]
  Io {
    context: String,
    #[source]
    source: std::io::Error,
  },
}

/// Result alias for `oci-layer` operations.
pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn io(context: impl fmt::Display) -> impl FnOnce(std::io::Error) -> Error {
  move |source| Error::Io {
    context: context.to_string(),
    source,
  }
}

#[cfg(test)]
#[path = "lib_test.rs"]
mod tests;
