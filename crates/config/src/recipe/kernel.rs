//! The operating system a recipe presents: kernel identity, hostname and
//! uptime. The software an actor runs is its image's, not the recipe's.

use serde::Deserialize;

use crate::value::{Duration, Template};

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
// NOTICE: `image` was removed (E2 in the images and run data spec,
// `docs/ai/references/architecture/2026-11-17-images-and-run-data-spec.md`):
// an image definition's `root:` names it, and `recipe::load` refuses
// `os.image` naming `root:`.
pub struct Os {
  pub kernel: Kernel,
  /// `{hash}` derives from the run seed.
  pub hostname: Option<Template>,
  /// Linux `/etc/machine-id` (32 lowercase hex chars). When absent, derived
  /// deterministically from the run or actor seed.
  pub machine_id: Option<Template>,
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
