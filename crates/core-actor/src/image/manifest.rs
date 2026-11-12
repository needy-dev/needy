//! A VM image's `manifest.json`: what the image is (needy version, driver,
//! architecture, base ABI) and where it came from, written by whatever put
//! the image in place (`needy image pull`, `needy image build`). The VM
//! drivers read its base ABI before they boot the image
//! ([`super::vm`]); `needy image ls` shows the rest.
//!
//! A release archive carries a manifest without `source` and `sha256`
//! (the release workflow writes it next to the image's files); `needy image
//! pull` fills both in when it unpacks the archive.
//!
//! See `docs/ai/references/architecture/2026-11-12-actor-images-design.md`.

use std::path::{Path, PathBuf};

use needy_config::profile::ActorDriver;
use serde::{Deserialize, Serialize};

/// The manifest's file name in a VM image's directory.
pub const FILE: &str = "manifest.json";

/// What a VM image is and where it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
  /// The needy version whose release, or whose `needy image build`, made it.
  /// It need not be the version that boots it: the base ABI decides that.
  pub version: String,
  /// The VM driver that boots it: `linux-firecracker` or `macos-vz`.
  pub driver: ActorDriver,
  /// The architecture of its kernel and root file system, as Rust and
  /// `uname -m` name it (`x86_64`, `aarch64`).
  pub arch: String,
  /// The base ABI it was built for ([`super::base::ABI`]).
  pub base_abi: u32,
  /// Where it came from; `None` in a release archive, before `needy image
  /// pull` records it.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub source: Option<Source>,
  /// The SHA-256 of the archive it was unpacked from, as lowercase hex;
  /// `None` for an image built on this machine, which came from no archive.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub sha256: Option<String>,
}

impl Manifest {
  /// The manifest of a VM image of `driver` and `base_abi` that this needy
  /// made on this machine from `source`: of this needy's version and the
  /// driver's architecture, from no archive.
  pub fn made(driver: ActorDriver, base_abi: u32, source: Source) -> Self {
    Self {
      version: env!("CARGO_PKG_VERSION").into(),
      driver,
      arch: super::vm::arch(driver).into(),
      base_abi,
      source: Some(source),
      sha256: None,
    }
  }
}

/// Where a VM image came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Source {
  /// Downloaded from a release: its tag, and the archive's URL.
  Release { tag: String, url: String },
  /// Imported from an archive on this machine (`needy image pull --file`).
  File { path: PathBuf },
  /// Built from a needy checkout by `needy image build`, with the recipe
  /// whose kernel options it has, if any.
  Build {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recipe: Option<String>,
  },
  /// Made by `needy image build --from` from an OCI image: the reference as
  /// named, with its tag, the image's ID (the digest of its configuration)
  /// and its platform (`linux/amd64`, `linux/arm64`).
  Oci {
    reference: String,
    id: String,
    platform: String,
  },
}

impl std::fmt::Display for Source {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Source::Release { tag, .. } => write!(f, "release {tag}"),
      Source::File { path } => write!(f, "file {}", path.display()),
      Source::Build { recipe: None } => write!(f, "built"),
      Source::Build {
        recipe: Some(recipe),
      } => write!(f, "built with recipe {recipe}"),
      Source::Oci { reference, id, .. } => write!(f, "built from {reference} ({id})"),
    }
  }
}

/// A manifest that could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("cannot read {}: {source}", path.display())]
  Read {
    path: PathBuf,
    #[source]
    source: std::io::Error,
  },
  #[error("{} is not a VM image manifest: {message}", path.display())]
  Invalid { path: PathBuf, message: String },
  #[error("cannot write {}: {source}", path.display())]
  Write {
    path: PathBuf,
    #[source]
    source: std::io::Error,
  },
}

/// The manifest of the VM image in `dir`, or `None` when it has none.
pub fn read(dir: &Path) -> Result<Option<Manifest>, Error> {
  let path = dir.join(FILE);
  let bytes = match std::fs::read(&path) {
    Ok(bytes) => bytes,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
    Err(source) => return Err(Error::Read { path, source }),
  };
  serde_json::from_slice(&bytes).map(Some).map_err(|e| Error::Invalid {
    path,
    message: e.to_string(),
  })
}

/// Writes `manifest` into the VM image directory `dir`.
pub fn write(dir: &Path, manifest: &Manifest) -> Result<(), Error> {
  let path = dir.join(FILE);
  let mut json = serde_json::to_vec_pretty(manifest).map_err(|e| Error::Invalid {
    path: path.clone(),
    message: e.to_string(),
  })?;
  json.push(b'\n');
  std::fs::write(&path, json).map_err(|source| Error::Write { path, source })
}

#[cfg(test)]
#[path = "manifest_test.rs"]
mod tests;
