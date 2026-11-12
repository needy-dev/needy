//! needy's VM actor images: a kernel and a root file system in one
//! directory, for the VM drivers. `linux-firecracker` boots the x86_64
//! kernel with the patched Firecracker beside it; `macos-vz` boots the arm64
//! kernel with an empty initramfs.
//!
//! An image is a base image (`needy image pull`, or `needy image build` in a
//! needy checkout), or one `needy image build --from REF` made from an OCI
//! image `FROM` the container base: its root file system is the OCI
//! image's, and its other files are the base's. Neither holds a needy of its
//! own: the VM drivers attach the guest needy as a second disk
//! ([`super::guest::disk`]). Every image directory has a
//! [`super::manifest`], which records the base ABI it was built for.
//!
//! needy's data directory keeps them by driver, in `images/<driver>/`: the
//! base in [`base_dir`], and each image made from an OCI image in
//! [`dir_of`]. A profile names one by the OCI reference it was made from
//! (`actor.image`), the same reference the container drivers run.

use std::path::{Path, PathBuf};

use needy_config::profile::ActorDriver;
use sha2::{Digest as _, Sha256};

use super::{base, manifest};
use crate::driver::Error;

/// Where an image's files come from, for what a message advises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
  /// The base image `needy image pull` or `needy image build` writes.
  Base,
  /// The image `needy image build --from REFERENCE` made.
  Reference(String),
}

impl Source {
  /// What to run when `driver`'s image is missing: the base is pulled
  /// first and built from a checkout second; an image made from an OCI
  /// image is made from it again.
  fn missing(&self, driver: ActorDriver) -> String {
    match self {
      Source::Base => format!("run `needy image pull --driver {driver}`, or `needy image build` in a needy checkout"),
      Source::Reference(reference) => format!("run `needy image build --from {reference}`"),
    }
  }

  /// How to make an image of this needy's base ABI.
  fn advice(&self, driver: ActorDriver) -> String {
    match self {
      Source::Base => base::pull_or_build(&format!("needy image pull --driver {driver}"), "needy image build"),
      Source::Reference(reference) => {
        base::rebuild_from("needy image build --linux-container", Some(&format!("needy image build --from {reference}")))
      }
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

/// This host's VM driver: `macos-vz` on a Mac with Apple silicon,
/// `linux-firecracker` anywhere else (where it may still lack KVM).
pub fn host_driver() -> ActorDriver {
  if crate::driver::macos_vz::supported_host() {
    ActorDriver::MacosVz
  } else {
    ActorDriver::LinuxFirecracker
  }
}

/// The architecture of `driver`'s VM images, as Rust and `uname -m` name
/// it: the Firecracker actor is x86_64 only, the `macos-vz` actor arm64 only
/// (`images/Dockerfile`).
pub fn arch(driver: ActorDriver) -> &'static str {
  match driver {
    ActorDriver::MacosVz => "aarch64",
    _ => "x86_64",
  }
}

/// The directory in `images` (needy's data directory's `images`) of
/// `driver`'s base VM image, which `needy image pull` and `needy image
/// build` write and a VM driver boots for a profile without `actor.image`.
// NOTICE: `base` never collides with a [`dir_of`] name, which always ends in
// `-` and 12 hex digits.
pub fn base_dir(images: &Path, driver: ActorDriver) -> PathBuf {
  images.join(driver.as_str()).join("base")
}

/// The directory in `images` (needy's data directory's `images`) of the VM
/// image `needy image build --from REFERENCE` made for `driver`: one
/// directory a driver, and in it one an image, named after the tagged
/// reference (`needy-agent:latest` for `needy-agent`), its characters a file
/// name cannot hold replaced, and the first 12 hex digits of the
/// reference's SHA-256, so that two references never share one.
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

/// Fails with the first of `files` that is missing, unless the image in
/// `dir` has a manifest of `driver` that records this needy's base ABI.
fn check(dir: &Path, driver: ActorDriver, source: &Source, files: &[&PathBuf]) -> Result<(), Error> {
  if let Some(missing) = files.iter().find(|path| !path.exists()) {
    return Err(Error::Missing {
      path: (*missing).clone(),
      advice: source.missing(driver),
    });
  }
  let Some(manifest) = manifest::read(dir)? else {
    return Err(Error::Missing {
      path: dir.join(manifest::FILE),
      advice: source.missing(driver),
    });
  };
  if manifest.driver != driver {
    return Err(Error::driver(
      driver,
      format!("the VM image {} is a {} image, not a {driver} one; {}", dir.display(), manifest.driver, source.missing(driver)),
    ));
  }
  Ok(base::check(&source.name(dir), Some(&manifest.base_abi.to_string()), &source.advice(driver))?)
}

/// The files of a `linux-firecracker` VM image.
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

  /// Fails with the first file the image lacks, or unless its manifest
  /// records this needy's base ABI.
  pub fn check(&self) -> Result<(), Error> {
    check(&self.dir, ActorDriver::LinuxFirecracker, &self.source, &[&self.kernel, &self.rootfs, &self.firecracker])
  }
}

/// The files of a `macos-vz` VM image.
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

  /// Fails with the first file the image lacks, or unless its manifest
  /// records this needy's base ABI.
  pub fn check(&self) -> Result<(), Error> {
    check(&self.dir, ActorDriver::MacosVz, &self.source, &[&self.kernel, &self.initrd, &self.rootfs])
  }
}

/// The files of `driver`'s VM image, the manifest aside: the root file
/// system first.
pub fn files(driver: ActorDriver) -> &'static [&'static str] {
  match driver {
    ActorDriver::LinuxFirecracker => &["rootfs.ext4", "vmlinux", "firecracker"],
    ActorDriver::MacosVz => &["rootfs.ext4", "Image", "initrd"],
    ActorDriver::LinuxContainer | ActorDriver::MacosAppleContainer => &[],
  }
}

/// The files of `driver`'s VM image other than the root file system, which
/// an image made from an OCI image takes from the base: the kernel, and
/// Firecracker or the initramfs.
pub fn base_files(driver: ActorDriver) -> &'static [&'static str] {
  files(driver).get(1..).unwrap_or_default()
}

#[cfg(test)]
#[path = "vm_test.rs"]
mod tests;
