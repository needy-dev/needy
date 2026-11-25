//! The root an image definition's steps are built on, as step cache keys and
//! `BridgeAttached.root` name it: the stack digest of needy's `actor-base`,
//! the OCI ChainID of its layers ([`oci_layer::chain_id`]). It is the same
//! whichever builder built a step, a VM session on the base VM image derived
//! from `actor-base` or a container of `needy-actor` (`actor-base` and one
//! shim layer), so a layer built by one is reused by the other, and a remote
//! actor runner compares it with the base it boots.
//!
//! An image that records no base keeps the identity needy gave it before:
//! the base VM image's archive digest or file digest, or the engine's ID of
//! a container image.
//!
//! The base's layers are what one build of `actor-base` made: the needy
//! layer is not reproducible across builders (a local engine, the release's
//! buildx), so a base built from a checkout has its own identity, and the
//! layers built on it are not shared with a release's.
//!
//! See B2 of `docs/ai/references/architecture/2026-11-25-layered-oci-base-spec.md`.

use needy_core::image::{Digest, is_needy_base_image};
use needy_core_executor::engine;

use super::manifest::Manifest;

/// The label a container image names the image it was built from in.
// Reference: https://github.com/opencontainers/image-spec/blob/v1.1.0/annotations.md#pre-defined-annotation-keys
pub const BASE_NAME_LABEL: &str = "org.opencontainers.image.base.name";

/// A root that cannot be told.
#[derive(Debug, thiserror::Error)]
pub enum Error {
  /// The base an image records names no layers.
  #[error("the base {image} records no layers")]
  NoLayers { image: String },
  /// The base an image records names a layer by something that is not a
  /// DiffID.
  #[error("the base {image} records the layer {diff_id:?}, which is not a sha256 DiffID")]
  InvalidLayer { image: String, diff_id: String },
  /// A container image named as needy's base is not shaped as `needy-actor`
  /// is, so which of its layers are `actor-base`'s is unknown.
  #[error(
    "cannot tell which layers of the container actor image {image} are its base: {reason}; pull this needy's `needy-actor` with \
     `needy image pull --driver linux-container`, or rebuild it with `needy image build --linux-container` in a needy checkout"
  )]
  Undetermined { image: String, reason: String },
}

/// The stack digest of the base `image` whose layers' DiffIDs are
/// `diff_ids`, lowest first: their ChainID.
pub fn stack_digest(image: &str, diff_ids: &[String]) -> Result<Digest, Error> {
  let layers = diff_ids
    .iter()
    .map(|diff_id| {
      oci_layer::Digest::parse(diff_id).map_err(|_| Error::InvalidLayer {
        image: image.into(),
        diff_id: diff_id.clone(),
      })
    })
    .collect::<Result<Vec<_>, _>>()?;
  let chain = oci_layer::chain_id(&layers).ok_or_else(|| Error::NoLayers {
    image: image.into(),
  })?;
  Ok(Digest::from_bytes(*chain.as_bytes()))
}

/// The root of the VM image whose manifest is `manifest`: its base's stack
/// digest, or, for an image that records no base, the digest of the archive
/// it was unpacked from. `None` when it records neither, an image built
/// before needy recorded its base; its root is the digest of its
/// `rootfs.ext4`, which the caller hashes.
// TODO(base-identity): the archive and file digests are kept so the step
// caches of VM images made before B-1 (and of `needy image build --from`
// images, which have no base) keep working; drop them, and refuse an image
// without a base, once B-4 makes `--from` an import above the base and no
// supported release ships a manifest without `base`.
pub fn of_vm(manifest: &Manifest) -> Result<Option<Digest>, Error> {
  if let Some(base) = &manifest.base {
    return stack_digest(&base.image, &base.diff_ids).map(Some);
  }
  Ok(manifest.sha256.as_deref().and_then(|sha256| Digest::parse(&format!("sha256:{sha256}")).ok()))
}

/// The root of the container image `name`, which the engine describes as
/// `image`, for steps built in containers of it.
///
/// `needy-actor` (an image named as needy's base) is `actor-base` and one
/// shim layer, and its [`BASE_NAME_LABEL`] names an `actor-base` (empty
/// when built outside the release): its root is the stack digest of its
/// layers without the last. One that has the label but not that shape is
/// refused rather than guessed at.
pub fn of_container(name: &str, image: &engine::Image) -> Result<Digest, Error> {
  // NOTICE: a custom root (`root: debian:bookworm`, or an image `FROM
  // needy-actor`, which inherits its labels) has no base of needy's whose
  // layers it is known to start with; its root stays the image's ID.
  // TODO(base-identity): recognise an image that starts with `actor-base`'s
  // layers by comparing DiffIDs, with B-4's import above the base.
  if !is_needy_base_image(name) {
    return Ok(image_id(image));
  }
  // TODO(base-identity): a `needy-actor` from before B-1 is one flattened
  // layer without the label and records no base; it keeps the image's ID
  // until no supported release ships such an image, then refuse it.
  let Some(base) = image.labels.get(BASE_NAME_LABEL) else {
    return Ok(image_id(image));
  };
  let undetermined = |reason: String| Error::Undetermined {
    image: name.into(),
    reason,
  };
  if !base.is_empty() && !names_actor_base(base) {
    return Err(undetermined(format!("its {BASE_NAME_LABEL} label names {base}, not an actor-base image")));
  }
  match image.diff_ids.split_last() {
    Some((_shims, base_layers)) if !base_layers.is_empty() => stack_digest(name, base_layers),
    _ => Err(undetermined(format!("it has {} layers, and needy-actor is actor-base's layers and one shim layer", image.diff_ids.len()))),
  }
}

/// Whether `reference` names an `actor-base` image: the release's
/// `ghcr.io/needy-dev/needy/actor-base` or the `needy-actor-base` that
/// `needy image build` tags, at any registry, tag or digest.
fn names_actor_base(reference: &str) -> bool {
  let name = needy_config::image::untagged(reference);
  let last = name.rsplit('/').next().unwrap_or(name);
  last == "actor-base" || last == "needy-actor-base"
}

/// The engine's ID of `image` (the digest of its configuration) as a root,
/// the identity of an image that records no base.
fn image_id(image: &engine::Image) -> Digest {
  Digest::parse(&image.id).unwrap_or_else(|_| Digest::of(image.id.as_bytes()))
}

#[cfg(test)]
#[path = "root_test.rs"]
mod tests;
