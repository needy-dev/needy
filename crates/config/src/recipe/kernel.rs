//! The operating system a recipe presents: rootfs image, kernel identity,
//! hostname and uptime.

use serde::Deserialize;

use crate::value::{Duration, Template};

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Os {
  /// The OCI image the rootfs is built from.
  pub image: Option<String>,
  pub kernel: Kernel,
  /// `{hash}` derives from the session seed.
  pub hostname: Option<Template>,
  pub uptime: Option<Duration>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Kernel {
  /// `uname -r`.
  pub release: Option<String>,
  /// `uname -v`.
  pub version: Option<String>,
  pub build: KernelBuild,
}

/// What `/proc/version` says about the kernel's build.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KernelBuild {
  pub user: Option<String>,
  pub host: Option<String>,
  pub timestamp: Option<String>,
}
