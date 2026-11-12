//! The base ABI: the interface between a base image (kernel, root file
//! system, boot stub and shim stub) and the guest needy and daemon that use
//! it. Its parts are the paths in [`crate::contract`], the stubs' hand-off
//! (`images/stub/`), the guest disk ([`super::guest`]) and `binfmt_needy`'s
//! parameters and never-route rule. An image records the version it was
//! built for; a driver refuses an image whose version this needy does not
//! speak, before the actor starts.
//!
//! See `docs/ai/references/architecture/2026-11-12-actor-images-design.md`.

use std::path::Path;

/// The base ABI version this needy speaks. Raise it with any change to the
/// parts above, and with it what `images/Dockerfile` records.
pub const ABI: u32 = 1;

/// The file in a VM image's directory that records its version.
pub const FILE: &str = "base-abi";

/// The label a container image records its version in.
pub const LABEL: &str = "needy.base-abi";

/// An image this needy cannot start an actor from.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", self.message())]
pub struct Mismatch {
  /// The image: a VM image's directory, or a container image's name.
  pub image: String,
  /// The version it records, as written; `None` when it records none.
  pub found: Option<String>,
  /// The command that builds an image of this needy's version.
  pub rebuild: &'static str,
}

impl Mismatch {
  fn message(&self) -> String {
    let Mismatch {
      image,
      found,
      rebuild,
    } = self;
    match found {
      None => format!(
        "the actor image {image} records no base ABI: it was built before needy injected its guest at start, with a needy of its own \
         inside; rebuild it with `{rebuild}`"
      ),
      Some(found) => format!(
        "the actor image {image} has base ABI {found}, and this needy speaks base ABI {ABI}; rebuild it with `{rebuild}` from this \
         needy's version, or use a needy that speaks base ABI {found}"
      ),
    }
  }
}

/// Fails unless `found`, the version image `image` records (`None` when it
/// records none), is [`ABI`].
pub fn check(image: &str, found: Option<&str>, rebuild: &'static str) -> Result<(), Mismatch> {
  match found.map(str::trim) {
    Some(version) if version.parse() == Ok(ABI) => Ok(()),
    found => Err(Mismatch {
      image: image.to_string(),
      found: found.map(String::from),
      rebuild,
    }),
  }
}

/// Fails unless the VM image in `dir` records [`ABI`] in its [`FILE`].
pub fn check_dir(dir: &Path, rebuild: &'static str) -> Result<(), Mismatch> {
  let found = std::fs::read_to_string(dir.join(FILE)).ok();
  check(&dir.display().to_string(), found.as_deref(), rebuild)
}

#[cfg(test)]
#[path = "base_test.rs"]
mod tests;
