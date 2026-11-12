//! The container actor image as the container drivers run it: one container
//! per run, named after the run. A profile's `actor.image` names it, else
//! the base image of this needy's release
//! (`needy_config::profile::CONTAINER_IMAGE`), which the drivers pull when
//! it is not here; `needy image build --linux-container` or
//! `--macos-apple-container` builds the base from a checkout
//! (`needy_config::profile::BUILT_CONTAINER_IMAGE`). A custom image is an
//! OCI image `FROM` a base.
// NOTICE: the image's shim layout (`/.needy/proxy`, `/.needy/real/`) is in
// `crate::contract` (`PROXY`, `REAL`); the shim capture that reads it is
// `guest/capture/shim.rs`.

use super::base;

/// The container of run `run`'s actor.
pub fn name(run: &str) -> String {
  format!("needy-actor-{run}")
}

/// How to make an image of this needy's base ABI in place of `image`:
/// rebuild the locally built base with `build_base`, or rebuild any other
/// image `FROM` a base of this needy's base ABI.
pub fn advice(image: &str, build_base: &str) -> String {
  if image == needy_config::profile::BUILT_CONTAINER_IMAGE {
    base::rebuild_with(build_base)
  } else {
    base::rebuild_from(build_base, None)
  }
}

/// Why a container driver cannot start actors from `image`: it is not in
/// the engine's (or Apple's) store, and pulling it failed with `why`.
/// The release's base names how to build one when there is no release to
/// pull from; `build_base` builds it.
pub fn unpullable(image: &str, why: &str, build_base: &str) -> String {
  if image == needy_config::profile::CONTAINER_IMAGE {
    format!(
      "the container actor image {image} is not here and cannot be pulled ({why}); a needy without a release (a development \
       build) has none to pull: build the base with `{build_base}` and set `actor.image: {built}`",
      built = needy_config::profile::BUILT_CONTAINER_IMAGE
    )
  } else if image == needy_config::profile::BUILT_CONTAINER_IMAGE {
    format!("the container actor image {image} is not here and cannot be pulled ({why}); build it with `{build_base}`")
  } else {
    format!("the container actor image {image} is not here and cannot be pulled ({why}); build it, or check its reference")
  }
}

/// The OCI name of `arch` (`std::env::consts::ARCH`): the image
/// configuration's `architecture`, which is Go's `GOARCH`.
// Reference: https://github.com/opencontainers/image-spec/blob/v1.1.1/config.md#properties
// (`architecture`: "values SHOULD be listed in the Go Language document for
// GOARCH") and https://go.dev/doc/install/source#environment (no versioned
// page; read 2026-11-13).
pub fn oci_arch(arch: &str) -> &str {
  match arch {
    "x86_64" => "amd64",
    "aarch64" => "arm64",
    other => other,
  }
}

/// Fails unless an image of platform `os`/`architecture` can run actors
/// whose guest needy is for `arch`: the guest needy and the shims run its
/// programs, so its architecture must be theirs, whatever emulation the
/// engine has.
pub fn check_platform(image: &str, os: &str, architecture: &str, arch: &str) -> Result<(), String> {
  let expected = oci_arch(arch);
  if os == "linux" && architecture == expected {
    return Ok(());
  }
  Err(format!(
    "the actor image {image} is {os}/{architecture}, and actors here run linux/{expected}, the guest needy's platform; \
     build it for linux/{expected} (`docker build --platform linux/{expected}`)"
  ))
}

#[cfg(test)]
#[path = "container_test.rs"]
mod tests;
