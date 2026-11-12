//! The base ABI: the interface between a base image (kernel, root file
//! system, boot stub and shim stub) and the guest needy and daemon that use
//! it. Its parts are the paths in [`crate::contract`], the stubs' hand-off
//! (`images/stub/`), the guest disk ([`super::guest`]) and `binfmt_needy`'s
//! parameters and never-route rule. An image records the version it was
//! built for; a driver refuses an image whose version this needy does not
//! speak, before the actor starts.
//!
//! See `docs/ai/references/architecture/2026-11-12-actor-images-design.md`.

/// The base ABI version this needy speaks. Raise it with any change to the
/// parts above, and with it what `images/Dockerfile` records.
pub const ABI: u32 = 1;

/// The file `images/Dockerfile`'s VM image targets export next to the
/// image's files, with the version. `needy image build` and the release
/// workflow fold it into the image's [`super::manifest`], which is what a
/// VM driver reads; an image directory does not keep it.
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
  /// What makes an image of this needy's version: [`rebuild_with`] or
  /// [`rebuild_from`].
  pub advice: String,
}

impl Mismatch {
  fn message(&self) -> String {
    let Mismatch {
      image,
      found,
      advice,
    } = self;
    match found {
      None => format!(
        "the actor image {image} records no base ABI: it is not built FROM a needy base image, or was built \
         before needy injected its guest at start; {advice}"
      ),
      Some(found) => format!(
        "the actor image {image} has base ABI {found}, and this needy speaks base ABI {ABI}; {advice}, or use a needy that \
         speaks base ABI {found}"
      ),
    }
  }
}

/// The advice for a base image that `pull` fetches from this needy's
/// release and `build` builds from a needy checkout.
pub fn pull_or_build(pull: &str, build: &str) -> String {
  format!("pull this needy's base with `{pull}`, or rebuild it with `{build}` in a needy checkout")
}

/// The advice for a base image, which `command` builds.
pub fn rebuild_with(command: &str) -> String {
  format!("rebuild it with `{command}` from this needy's version")
}

/// The advice for a custom image, an OCI image `FROM` a base: rebuild it
/// from a base of this needy's base ABI, the local one `build_base` builds
/// or the release's, and then run `then`, if any.
pub fn rebuild_from(build_base: &str, then: Option<&str>) -> String {
  let then = then.map(|then| format!(", then run `{then}`")).unwrap_or_default();
  format!(
    "rebuild it FROM a base image of base ABI {ABI}: `{local}` after `{build_base}`, or `{release}`{then}",
    local = needy_config::profile::BUILT_CONTAINER_IMAGE,
    release = needy_config::profile::CONTAINER_IMAGE,
  )
}

/// Fails unless `found`, the version image `image` records (`None` when it
/// records none), is [`ABI`]; `advice` says how to make one that does.
pub fn check(image: &str, found: Option<&str>, advice: &str) -> Result<(), Mismatch> {
  match found.map(str::trim) {
    Some(version) if version.parse() == Ok(ABI) => Ok(()),
    found => Err(Mismatch {
      image: image.to_string(),
      found: found.map(String::from),
      advice: advice.to_string(),
    }),
  }
}

#[cfg(test)]
#[path = "base_test.rs"]
mod tests;
