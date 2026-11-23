//! Embedded built-in Dev Container features bundled with needy.
//!
//! Kept strictly outside the standalone `devcontainer-features` crate so
//! `devcontainer-features` remains 100% agent-agnostic and reusable.

use devcontainer_features::{Digest, FetchError};

/// Filename of the feature metadata document inside a Dev Container feature tar.
const METADATA_FILE: &str = "devcontainer-feature.json";

/// A built-in Dev Container feature bundled with needy rather than fetched
/// from an OCI registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinFeature {
  /// Stable reference name (for example `builtin:sshfs`).
  pub reference: &'static str,
  /// Raw `devcontainer-feature.json` metadata document.
  pub metadata_json: &'static str,
  /// POSIX `install.sh` script.
  pub install_sh: &'static str,
  /// Additional files bundled alongside `install.sh` as `(name, mode, bytes)`.
  pub extra_files: &'static [(&'static str, u32, &'static [u8])],
}

impl BuiltinFeature {
  /// Packs this built-in feature into an uncompressed tar archive containing
  /// `devcontainer-feature.json`, an executable `install.sh`, and any
  /// `extra_files`.
  pub fn archive(&self) -> Result<Vec<u8>, FetchError> {
    let mut tar = tar::Builder::new(Vec::new());
    let base = [
      (METADATA_FILE, 0o644u32, self.metadata_json.as_bytes()),
      ("install.sh", 0o755u32, self.install_sh.as_bytes()),
    ];
    for &(name, mode, content) in base.iter().chain(self.extra_files.iter()) {
      let mut header = tar::Header::new_gnu();
      header.set_size(content.len() as u64);
      header.set_mode(mode);
      header.set_cksum();
      tar
        .append_data(&mut header, name, content)
        .map_err(|e| FetchError::InvalidArchive(format!("cannot pack built-in feature `{}`: {e}", self.reference)))?;
    }
    tar.into_inner().map_err(|e| FetchError::InvalidArchive(format!("cannot finalize built-in feature `{}`: {e}", self.reference)))
  }

  /// Content digest of this built-in feature's metadata, install script, and
  /// bundled extra files.
  pub fn digest(&self) -> Digest {
    let mut bytes = Vec::with_capacity(self.metadata_json.len() + self.install_sh.len() + 1);
    bytes.extend_from_slice(self.metadata_json.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(self.install_sh.as_bytes());
    for &(name, mode, content) in self.extra_files {
      bytes.push(0);
      bytes.extend_from_slice(name.as_bytes());
      bytes.push(0);
      bytes.extend_from_slice(&mode.to_le_bytes());
      bytes.push(0);
      bytes.extend_from_slice(content);
    }
    Digest::of(&bytes)
  }
}

/// Embedded built-in Dev Container features shipped with needy so `builtin:<id>`
/// references work offline without an external OCI registry or repository clone.
const BUILTINS: &[BuiltinFeature] = &[BuiltinFeature {
  reference: "builtin:sshfs",
  metadata_json: include_str!("../../../../features/sshfs/devcontainer-feature.json"),
  install_sh: include_str!("../../../../features/sshfs/install.sh"),
  extra_files: &[],
}];

/// Returns whether `reference` names a built-in needy feature (`builtin:<id>`).
pub fn is_builtin_ref(reference: &str) -> bool {
  reference.starts_with("builtin:")
}

/// Returns all built-in Dev Container features embedded in needy.
pub fn all_builtins() -> &'static [BuiltinFeature] {
  BUILTINS
}

/// Looks up an embedded built-in Dev Container feature by reference (for
/// example `builtin:sshfs`).
pub fn builtin(reference: &str) -> Option<BuiltinFeature> {
  let untagged = reference.split_once('@').map_or(reference, |(name, _)| name);
  BUILTINS.iter().find(|f| f.reference == untagged).cloned()
}

/// Needy's built-in `sshfs` Dev Container feature (decision E8 in
/// `docs/ai/references/architecture/2026-11-17-images-and-run-data-spec.md`):
/// checks `command -v sshfs` and installs `sshfs` via the root's package
/// manager when absent.
pub fn builtin_sshfs() -> BuiltinFeature {
  builtin("builtin:sshfs").expect("builtin:sshfs is embedded")
}
