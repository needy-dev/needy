//! needy's VM actor images: a kernel and a root file system in one
//! directory, for the VM drivers. `linux-firecracker` boots the x86_64
//! kernel with the patched Firecracker beside it; `macos-vz` boots the arm64
//! kernel with an empty initramfs.
//!
//! An image is a base image (`needy build`), or one `needy build --from REF`
//! made from an OCI image `FROM` the container base: its root file system is
//! the OCI image's, and its other files are the base's. Neither holds a
//! needy of its own: the VM drivers attach the guest needy as a second disk
//! ([`super::guest::disk`]), and the image records the base ABI it was built
//! for ([`super::base::FILE`]).
//!
//! A profile names a VM image by the OCI reference it was made from
//! (`actor.image`), the same reference the container drivers run, and
//! [`dir_of`] says where `needy build --from` put it.

use std::path::{Path, PathBuf};

use needy_config::profile::ActorDriver;
use sha2::{Digest as _, Sha256};

use super::base;
use crate::driver::Error;

/// The file in a VM image made from an OCI image that says which one
/// ([`Origin`]).
// TODO(actor-images): step 4's `manifest.json` (version, driver, arch,
// base_abi, source, sha256) replaces this file and `base-abi`, see
// docs/ai/references/architecture/2026-11-12-actor-images-design.md.
pub const ORIGIN: &str = "origin.json";

/// The OCI image a VM image was made from, as `needy build --from` records
/// it in [`ORIGIN`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Origin {
  /// The reference as `--from` named it, with its tag.
  pub reference: String,
  /// The image's ID (the digest of its configuration) when it was built.
  pub id: String,
  /// Its platform: `linux/amd64` or `linux/arm64`.
  pub platform: String,
}

/// Where an image's files come from, for what a message advises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
  /// The base image `needy build` writes.
  Base,
  /// The image `needy build --from REFERENCE` made.
  Reference(String),
}

impl Source {
  /// The command that writes the image.
  fn build(&self) -> String {
    match self {
      Source::Base => "needy build".into(),
      Source::Reference(reference) => format!("needy build --from {reference}"),
    }
  }

  /// How to make an image of this needy's base ABI.
  fn advice(&self) -> String {
    match self {
      Source::Base => base::rebuild_with("needy build"),
      Source::Reference(reference) => base::rebuild_from("needy build --linux-container", Some(&format!("needy build --from {reference}"))),
    }
  }

  /// The image, as a message names it.
  fn name(&self, dir: &Path) -> String {
    match self {
      Source::Base => dir.display().to_string(),
      Source::Reference(reference) => format!("{reference} (the VM image {})", dir.display()),
    }
  }
}

/// The directory in `images` (needy's data directory's `images`) of the VM
/// image `needy build --from REFERENCE` made for `driver`: one directory a
/// driver, as step 4's `images/<driver>/` has, and in it one an image, named
/// after the tagged reference (`needy-agent:latest` for `needy-agent`), its
/// characters a file name cannot hold replaced, and the first 12 hex digits
/// of the reference's SHA-256, so that two references never share one.
pub fn dir_of(images: &Path, driver: ActorDriver, reference: &str) -> PathBuf {
  let reference = needy_core_executor::engine::with_tag(reference);
  let readable: String = reference
    .chars()
    .map(|c| {
      if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
        c
      } else {
        '_'
      }
    })
    .collect();
  let digest = Sha256::digest(reference.as_bytes());
  let hex: String = digest.iter().take(6).map(|byte| format!("{byte:02x}")).collect();
  images.join(driver.as_str()).join(format!("{readable}-{hex}"))
}

/// Reads the [`Origin`] of the VM image in `dir`, or `None` when it has none
/// (a base image) or it cannot be read.
pub fn origin(dir: &Path) -> Option<Origin> {
  serde_json::from_slice(&std::fs::read(dir.join(ORIGIN)).ok()?).ok()
}

/// Fails with the first of `files` that is missing, or unless the image in
/// `dir` records this needy's base ABI.
fn check(dir: &Path, source: &Source, files: &[&PathBuf]) -> Result<(), Error> {
  match files.iter().find(|path| !path.exists()) {
    Some(missing) => Err(Error::Missing {
      path: (*missing).clone(),
      build: source.build(),
    }),
    None => Ok(base::check_dir(dir, &source.name(dir), &source.advice())?),
  }
}

/// The files `needy build` writes for `linux-firecracker` actors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxFirecracker {
  /// The image's directory.
  pub dir: PathBuf,
  pub kernel: PathBuf,
  pub rootfs: PathBuf,
  /// The patched Firecracker that applies a recipe's identity.
  pub firecracker: PathBuf,
  pub source: Source,
}

impl LinuxFirecracker {
  /// The base image in directory `dir`.
  pub fn new(dir: &Path) -> Self {
    Self::of(dir, Source::Base)
  }

  /// The image in directory `dir`, which `source` writes.
  pub fn of(dir: &Path, source: Source) -> Self {
    Self {
      dir: dir.to_path_buf(),
      kernel: dir.join("vmlinux"),
      rootfs: dir.join("rootfs.ext4"),
      firecracker: dir.join("firecracker"),
      source,
    }
  }

  /// Fails with the first file the image lacks, or unless it records this
  /// needy's base ABI.
  pub fn check(&self) -> Result<(), Error> {
    check(&self.dir, &self.source, &[&self.kernel, &self.rootfs, &self.firecracker])
  }
}

/// The files `needy build` writes for `macos-vz` actors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacosVz {
  /// The image's directory.
  pub dir: PathBuf,
  /// The uncompressed arm64 kernel (`arch/arm64/boot/Image`):
  /// Virtualization.framework's Linux boot loader takes no compressed one.
  pub kernel: PathBuf,
  /// An empty initramfs: vfkit needs one (see `images/Dockerfile`).
  pub initrd: PathBuf,
  pub rootfs: PathBuf,
  pub source: Source,
}

impl MacosVz {
  /// The base image in directory `dir`.
  pub fn new(dir: &Path) -> Self {
    Self::of(dir, Source::Base)
  }

  /// The image in directory `dir`, which `source` writes.
  pub fn of(dir: &Path, source: Source) -> Self {
    Self {
      dir: dir.to_path_buf(),
      kernel: dir.join("Image"),
      initrd: dir.join("initrd"),
      rootfs: dir.join("rootfs.ext4"),
      source,
    }
  }

  /// Fails with the first file the image lacks, or unless it records this
  /// needy's base ABI.
  pub fn check(&self) -> Result<(), Error> {
    check(&self.dir, &self.source, &[&self.kernel, &self.initrd, &self.rootfs])
  }
}

/// The files of `driver`'s VM image other than the root file system, which
/// an image made from an OCI image takes from the base: the kernel, and
/// Firecracker or the initramfs.
pub fn base_files(driver: ActorDriver) -> &'static [&'static str] {
  match driver {
    ActorDriver::LinuxFirecracker => &["vmlinux", "firecracker"],
    ActorDriver::MacosVz => &["Image", "initrd"],
    ActorDriver::LinuxContainer | ActorDriver::MacosAppleContainer => &[],
  }
}

#[cfg(test)]
#[path = "vm_test.rs"]
mod tests;
