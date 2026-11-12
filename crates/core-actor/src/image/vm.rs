//! needy's VM actor image: the kernel and root file system `needy build`
//! writes into one directory, for the VM drivers. `linux-firecracker` boots
//! the x86_64 kernel with the patched Firecracker beside it; `macos-vz` boots
//! the arm64 kernel with an empty initramfs.
//!
//! The image is a base image: it holds no needy of its own. The VM drivers
//! attach the guest needy as a second disk ([`super::guest::disk`]), and the
//! image records the base ABI it was built for ([`super::base::FILE`]).

use std::path::{Path, PathBuf};

use super::base;
use crate::driver::Error;

/// What builds a VM image of this needy's version.
const REBUILD: &str = "needy build";

/// The files `needy build` writes for `linux-firecracker` actors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxFirecracker {
  /// The image's directory.
  pub dir: PathBuf,
  pub kernel: PathBuf,
  pub rootfs: PathBuf,
  /// The patched Firecracker that applies a recipe's identity.
  pub firecracker: PathBuf,
}

impl LinuxFirecracker {
  /// The image in directory `dir`.
  pub fn new(dir: &Path) -> Self {
    Self {
      dir: dir.to_path_buf(),
      kernel: dir.join("vmlinux"),
      rootfs: dir.join("rootfs.ext4"),
      firecracker: dir.join("firecracker"),
    }
  }

  /// Fails with the first file the image lacks, or unless it records this
  /// needy's base ABI.
  pub fn check(&self) -> Result<(), Error> {
    match [&self.kernel, &self.rootfs, &self.firecracker].into_iter().find(|path| !path.exists()) {
      Some(missing) => Err(Error::Missing(missing.clone())),
      None => Ok(base::check_dir(&self.dir, REBUILD)?),
    }
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
}

impl MacosVz {
  /// The image in directory `dir`.
  pub fn new(dir: &Path) -> Self {
    Self {
      dir: dir.to_path_buf(),
      kernel: dir.join("Image"),
      initrd: dir.join("initrd"),
      rootfs: dir.join("rootfs.ext4"),
    }
  }

  /// Fails with the first file the image lacks, or unless it records this
  /// needy's base ABI.
  pub fn check(&self) -> Result<(), Error> {
    match [&self.kernel, &self.initrd, &self.rootfs].into_iter().find(|path| !path.exists()) {
      Some(missing) => Err(Error::Missing(missing.clone())),
      None => Ok(base::check_dir(&self.dir, REBUILD)?),
    }
  }
}
