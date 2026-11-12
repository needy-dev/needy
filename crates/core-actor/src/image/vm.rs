//! needy's VM actor image: the kernel and root file system `needy build`
//! writes into one directory, for the VM drivers. `linux-firecracker` boots
//! the x86_64 kernel with the patched Firecracker beside it; `macos-vz` boots
//! the arm64 kernel with an empty initramfs.

use std::path::{Path, PathBuf};

use crate::driver::Error;

/// The files `needy build` writes for `linux-firecracker` actors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxFirecracker {
  pub kernel: PathBuf,
  pub rootfs: PathBuf,
  /// The patched Firecracker that applies a recipe's identity.
  pub firecracker: PathBuf,
}

impl LinuxFirecracker {
  /// The image in directory `dir`.
  pub fn new(dir: &Path) -> Self {
    Self {
      kernel: dir.join("vmlinux"),
      rootfs: dir.join("rootfs.ext4"),
      firecracker: dir.join("firecracker"),
    }
  }

  /// Fails with the first file the image lacks.
  pub fn check(&self) -> Result<(), Error> {
    match [&self.kernel, &self.rootfs, &self.firecracker].into_iter().find(|path| !path.exists()) {
      Some(missing) => Err(Error::Missing(missing.clone())),
      None => Ok(()),
    }
  }
}

/// The files `needy build` writes for `macos-vz` actors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacosVz {
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
      kernel: dir.join("Image"),
      initrd: dir.join("initrd"),
      rootfs: dir.join("rootfs.ext4"),
    }
  }

  /// Fails with the first file the image lacks.
  pub fn check(&self) -> Result<(), Error> {
    match [&self.kernel, &self.initrd, &self.rootfs].into_iter().find(|path| !path.exists()) {
      Some(missing) => Err(Error::Missing(missing.clone())),
      None => Ok(()),
    }
  }
}
