//! Importing an OCI image built `FROM` needy's `actor-base` into the layer
//! store, so that a VM actor boots it as the base VM image with the image's
//! own layers stacked above it as layer disks (B4 of the layered OCI base
//! spec), as it boots an image definition's built steps.
//!
//! An import reads the image's manifest for the actor's platform (through
//! an image index when the reference names one) and its config, and checks
//! that:
//!
//! - the image records this needy's base ABI (its `needy.base-abi` label);
//! - its first layers are exactly the `actor-base` this needy's base VM image
//!   was derived from (its `rootfs.diff_ids` start with the base's DiffIDs,
//!   which the VM image's `manifest.json` records);
//! - no layer above the base writes, removes or hides `/.needy`, where the
//!   base keeps what the guest and the base ABI rely on.
//!
//! It pulls only the layers above the base, by digest: the registry's blob
//! is checked against its descriptor, the decompressed stream (gzip, zstd or
//! none) against the DiffID the config names, and the store keeps each
//! layer in its canonical form, named by its own digest, with the DiffID as
//! its alias ([`Store::imported`]), so the next import of the layer pulls
//! nothing. Empty layers (`ENV`, `WORKDIR` and other steps that change no
//! file, as envbuilder and kaniko write them) are skipped.
//!
//! What an import does not take from the image: its `Entrypoint`, `Cmd`,
//! `User` and `WorkingDir`. A VM actor's init is needy's, and the profile
//! says what the actor runs and as whom. Its `Env` is taken: `PATH` as the
//! image's `PATH`, the rest as the image's environment, as a feature's
//! `containerEnv` is.
//!
//! See `docs/ai/references/architecture/2026-11-25-layered-oci-base-spec.md`
//! (B4, "Settled in B-4").
// Reference: https://github.com/opencontainers/image-spec/blob/v1.1.0/image-index.md
// (platform selection), https://github.com/opencontainers/image-spec/blob/v1.1.0/manifest.md,
// https://github.com/opencontainers/image-spec/blob/v1.1.0/config.md (`rootfs.diff_ids`,
// lowest first) and https://github.com/opencontainers/image-spec/blob/v1.1.0/layer.md
// (media types, whiteouts).
// TODO(import-offline): every import reads the image's manifest and config
// from the registry, even for a reference pinned by digest whose layers the
// store holds; recording what an import found would let a pinned reference
// start without the registry. Reopen when a deployment runs actors without
// registry access.

#[cfg(any(test, feature = "test-support"))]
pub mod testing;
#[cfg(test)]
#[path = "import_test.rs"]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};
use std::str::FromStr;

use oci_client::errors::OciDistributionError;
use oci_client::manifest::{
  IMAGE_DOCKER_LAYER_GZIP_MEDIA_TYPE, IMAGE_DOCKER_LAYER_TAR_MEDIA_TYPE, IMAGE_LAYER_GZIP_MEDIA_TYPE, IMAGE_LAYER_MEDIA_TYPE,
  IMAGE_MANIFEST_LIST_MEDIA_TYPE, IMAGE_MANIFEST_MEDIA_TYPE, OCI_IMAGE_INDEX_MEDIA_TYPE, OCI_IMAGE_MEDIA_TYPE, OciDescriptor, OciImageIndex,
  OciImageManifest,
};
use oci_registry::{Access, Operation};
use oci_spec::image::ImageConfiguration;

use super::registry::{self, Compression};
use super::{Digest, Store};
use crate::image::{base, manifest, root};

/// The DiffID of an empty layer: an empty tar archive, two 512-byte blocks
/// of zeros. Builders write one for a step that changes no file.
// Source: `head -c 1024 /dev/zero | sha256sum`; the same digest is moby's
// `DigestSHA256EmptyTar`, https://github.com/moby/moby/blob/v28.0.0/layer/empty.go#L12
// Reference: POSIX.1-2017 `pax`, "ustar Interchange Format" (an archive
// ends with two 512-byte blocks of zeros).
pub const EMPTY_LAYER: &str = "sha256:5f70bf18a086007016e948b04aed3b82103a36bea41755b6cddfaf10ace3c6ef";

/// The zstd layer media type, which `oci-client` 0.15 has no constant for.
// Source: https://github.com/opencontainers/image-spec/blob/v1.1.0/media-types.md
pub const IMAGE_LAYER_ZSTD_MEDIA_TYPE: &str = "application/vnd.oci.image.layer.v1.tar+zstd";

/// The directory of the base the guest and the base ABI rely on
/// (`crate::contract`): no imported layer may write, remove or hide it.
const NEEDY_DIR: &str = ".needy";

/// Top-level directories that builders leave in the images they build, and
/// that an actor does not need: an import warns about them.
// Source: envbuilder v1.1.0 writes `/.envbuilder` (its binary, the image's
// metadata) into the image it pushes, observed in a Shipwright `BuildRun`
// of 2026-11-25 (layer list in the B-4 notes of the layered OCI base spec).
const BUILDER_TREES: &[(&str, &str)] = &[(".envbuilder", "envbuilder")];

/// What an import refused or could not do.
#[derive(Debug, thiserror::Error)]
pub enum Error {
  /// `actor.image` is not an image reference.
  #[error("{text:?} is not an image reference: {reason}")]
  Reference { text: String, reason: String },
  /// Registry access (credentials, TLS) could not be set up.
  #[error("{reference}: {source}")]
  Access {
    reference: String,
    #[source]
    source: oci_registry::Error,
  },
  /// The registry refused a request or could not be reached.
  #[error("{reference}: {source}")]
  Registry {
    reference: String,
    #[source]
    source: OciDistributionError,
  },
  /// What the registry holds is not an image needy can import.
  #[error("{reference} is not an image needy can import: {reason}")]
  Invalid { reference: String, reason: String },
  /// The image has no manifest for the actor's platform.
  #[error("{reference} has no image for {platform}; it has {found}")]
  Platform {
    reference: String,
    platform: String,
    found: String,
  },
  /// The image does not record this needy's base ABI.
  #[error(transparent)]
  Abi(#[from] base::Mismatch),
  /// The image is not built on the base this needy's VM image was derived
  /// from.
  #[error(
    "{reference} is not built on this needy's actor-base: {found}; a VM actor imports the layers above the base its VM image was \
     derived from, so build the image FROM {needs}"
  )]
  Base {
    reference: String,
    /// The base to build `FROM`, as a `FROM` line names it.
    needs: String,
    found: String,
  },
  /// The image is built `FROM` `needy-actor`, the container base: its first
  /// layer above `actor-base` is the container shim step.
  #[error(
    "{reference} is built FROM needy-actor, the container base, whose shim layer (layer {layer}) rewrites the shells under /.needy; \
     a VM actor needs no shims, so build the image FROM {needs} instead"
  )]
  ContainerBase {
    reference: String,
    needs: String,
    layer: Digest,
  },
  /// A layer above the base writes, removes or hides `/.needy`.
  #[error(
    "{reference}: layer {layer} {action} /{path}; /.needy holds what this needy's guest and base ABI rely on, and an image built \
     FROM actor-base must not change it{hint}"
  )]
  NeedyPath {
    reference: String,
    layer: Digest,
    action: &'static str,
    path: String,
    hint: &'static str,
  },
  /// A layer decompressed to another layer than the config names.
  #[error("{reference} holds the layer {actual}, not the {expected} its config names")]
  DiffId {
    reference: String,
    expected: Digest,
    actual: Digest,
  },
  /// The base VM image does not say which `actor-base` it was derived from.
  #[error("{0}")]
  BaseImage(String),
  /// Pulling or storing a layer failed.
  #[error(transparent)]
  Pull(#[from] registry::Error),
  /// The layer store could not take a layer.
  #[error(transparent)]
  Store(#[from] super::Error),
  /// A local file could not be read or written.
  #[error("{context}: {source}")]
  Io {
    context: String,
    #[source]
    source: std::io::Error,
  },
}

pub type Result<T> = std::result::Result<T, Error>;

fn io(context: impl std::fmt::Display) -> impl FnOnce(std::io::Error) -> Error {
  move |source| Error::Io {
    context: context.to_string(),
    source,
  }
}

/// An image reference as a profile's `actor.image` names it: an OCI
/// reference (`ghcr.io/org/agent:1`, `registry.example.com/agent@sha256:...`,
/// Docker Hub's short names), or with an `http://` prefix for a registry
/// reached over plain HTTP, which never gets credentials (as the layer
/// registry's `http://`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
  text: String,
  reference: oci_client::Reference,
  plain_http: bool,
}

impl Reference {
  pub fn parse(text: &str) -> Result<Self> {
    let bad = |reason: String| Error::Reference {
      text: text.to_string(),
      reason,
    };
    let (plain_http, rest) = match text.strip_prefix("http://") {
      Some(rest) => (true, rest),
      None if text.contains("://") => return Err(bad("the only scheme is `http://`, for a plain-HTTP registry".into())),
      None => (false, text),
    };
    let reference = oci_client::Reference::from_str(rest).map_err(|e| bad(e.to_string()))?;
    Ok(Self {
      text: text.to_string(),
      reference,
      plain_http,
    })
  }

  /// The registry host, with its port if any.
  pub fn registry(&self) -> &str {
    self.reference.registry()
  }

  /// The reference as written.
  pub fn as_str(&self) -> &str {
    &self.text
  }
}

impl std::fmt::Display for Reference {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(&self.text)
  }
}

/// What an import found and stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
  /// The reference as written.
  pub reference: String,
  /// The digest of the image manifest for the actor's platform.
  pub manifest: Digest,
  /// The actor's platform: `linux/amd64` or `linux/arm64`.
  pub platform: String,
  /// The root the layers sit on: the base's stack digest
  /// ([`root::stack_digest`]), as built steps name it.
  pub root: Digest,
  /// The layers above the base as the store keeps them, lowest first,
  /// empty layers left out: what a snapshot of the image stacks.
  pub layers: Vec<Digest>,
  /// How many of `layers` were pulled; the others were stored already.
  pub pulled: usize,
  /// How many layers above the base were empty, or repeated the layer
  /// below them, and are not in `layers`.
  pub skipped: usize,
  /// The image's `PATH` (its config's `Env`), if it sets one.
  pub path: Option<String>,
  /// The image's environment without `PATH` (its config's `Env`).
  pub env: BTreeMap<String, String>,
  /// What the image has that a VM actor ignores or does not need.
  pub warnings: Vec<String>,
}

/// The `actor-base` the base VM image of `driver` in `dir` was derived
/// from, which an image imported for that driver must start with. Blocking.
pub fn vm_base(dir: &Path, driver: needy_config::profile::ActorDriver) -> Result<manifest::Base> {
  let manifest = manifest::read(dir).map_err(|e| Error::BaseImage(e.to_string()))?.ok_or_else(|| {
    Error::BaseImage(format!(
      "the base VM image {} has no {}; get one with `needy image pull --driver {driver}`",
      dir.display(),
      manifest::FILE
    ))
  })?;
  if manifest.driver != driver {
    return Err(Error::BaseImage(format!(
      "the base VM image {} is a {} image, and a {driver} actor's image is imported above a {driver} base; get one with `needy image \
       pull --driver {driver}`",
      dir.display(),
      manifest.driver
    )));
  }
  // NOTICE: a base VM image without `base` (made before B-1) cannot have an
  // image imported above it: which layers are above `actor-base` is unknown,
  // so it is refused rather than guessed at; `needy image pull` replaces it.
  manifest.base.ok_or_else(|| {
    Error::BaseImage(format!(
      "the base VM image {} records no actor-base (it was made before needy recorded its base), so which layers of an image are \
       above actor-base cannot be told; get this needy's base VM image with `needy image pull --driver {driver}`, or build one with \
       `needy image build` in a needy checkout",
      dir.display()
    ))
  })
}

/// The OCI architecture of an actor platform's `uname -m` name.
fn oci_arch(arch: &str) -> &str {
  crate::image::container::oci_arch(arch)
}

/// Imports `reference` for an actor of architecture `arch` (`x86_64`,
/// `aarch64`) whose VM image was derived from `base`, into `store`,
/// reaching the registry through `access`.
pub async fn import(store: &Store, access: &Access, reference: &Reference, base: &manifest::Base, arch: &str) -> Result<Imported> {
  let name = reference.as_str();
  let invalid = |reason: String| Error::Invalid {
    reference: name.to_string(),
    reason,
  };
  let registry_error = |source| Error::Registry {
    reference: name.to_string(),
    source,
  };
  let platform = format!("linux/{}", oci_arch(arch));
  let root = root::stack_digest(&base.image, &base.diff_ids).map_err(|e| invalid(format!("this needy's base: {e}")))?;
  let connection = access
    .connect(reference.registry(), reference.plain_http, reference.reference.repository(), Operation::Pull)
    .await
    .map_err(|source| Error::Access {
      reference: name.to_string(),
      source,
    })?;
  let client = &connection.client;

  let (manifest_digest, image_manifest) = resolve(client, &connection.auth, reference, &platform).await?;
  let config_bytes = {
    let mut config = Vec::new();
    // NOTICE: `pull_blob` checks what it reads against the descriptor's
    // digest, so a config that does not match its manifest is refused.
    client.pull_blob(&reference.reference, &image_manifest.config, &mut config).await.map_err(registry_error)?;
    config
  };
  let config: ImageConfiguration = serde_json::from_slice(&config_bytes).map_err(|e| invalid(format!("its config: {e}")))?;
  let (os, architecture) = (config.os().to_string(), config.architecture().to_string());
  if os != "linux" || architecture != oci_arch(arch) {
    return Err(Error::Platform {
      reference: name.to_string(),
      platform,
      found: format!("{os}/{architecture}"),
    });
  }
  let labels = config.config().as_ref().and_then(|c| c.labels().clone()).unwrap_or_default();
  base::check(name, labels.get(base::LABEL).map(String::as_str), &rebuild_from(base))?;

  let diff_ids = config.rootfs().diff_ids();
  if diff_ids.len() != image_manifest.layers.len() {
    return Err(invalid(format!("its config names {} layers and its manifest {}", diff_ids.len(), image_manifest.layers.len())));
  }
  check_base(name, base, diff_ids, labels.get(root::BASE_NAME_LABEL).map(String::as_str))?;

  let mut imported = Imported {
    reference: name.to_string(),
    manifest: manifest_digest,
    platform,
    root,
    layers: Vec::new(),
    pulled: 0,
    skipped: 0,
    path: None,
    env: BTreeMap::new(),
    warnings: Vec::new(),
  };
  let from_container_base = labels.get(root::BASE_NAME_LABEL).is_some_and(|label| label.is_empty() || root::names_actor_base(label));
  let mut stacked: Vec<Digest> = Vec::new();
  let mut below: Option<Digest> = None;
  let mut warned = BTreeSet::new();
  for (at, (descriptor, diff_id)) in image_manifest.layers.iter().zip(diff_ids).enumerate().skip(base.diff_ids.len()) {
    let diff_id = Digest::parse(diff_id).map_err(|e| invalid(format!("layer {at}'s DiffID {diff_id}: {e}")))?;
    if diff_id.to_string() == EMPTY_LAYER || below == Some(diff_id) {
      // NOTICE: a layer stacked twice in a row is the layer once: what the
      // second writes, the first wrote already.
      imported.skipped += 1;
      continue;
    }
    if stacked.contains(&diff_id) {
      // TODO(import-repeated-layers): a layer that comes back after others
      // would be stacked twice, which the store's disks (one per digest) and
      // the guest's mounts cannot express; collapsing the run between them
      // into one layer would. No builder observed writes one; reopen when
      // an image needs it.
      return Err(invalid(format!("layer {diff_id} comes back at layer {at}, after other layers; needy stacks a layer once")));
    }
    // NOTICE: a layer stored by an earlier import passed its checks then;
    // its builder-tree warnings were given then too and are not repeated.
    let digest = match store.imported(&diff_id) {
      Some(digest) => digest,
      None => {
        let compression = compression(&descriptor.media_type).map_err(|reason| invalid(format!("layer {at} ({diff_id}): {reason}")))?;
        let pulled = pull(client, reference, descriptor, compression, &diff_id, store).await;
        let pulled = match pulled {
          Err(Error::NeedyPath { layer, .. }) if from_container_base && at == base.diff_ids.len() => {
            return Err(Error::ContainerBase {
              reference: name.to_string(),
              needs: needs(base),
              layer,
            });
          }
          pulled => pulled?,
        };
        imported.pulled += 1;
        for (tree, builder) in pulled.trees {
          if warned.insert(tree.clone()) {
            imported
              .warnings
              .push(format!("layer {diff_id} has /{tree}, which {builder} leaves in the images it builds; a VM actor does not use it"));
          }
        }
        pulled.digest
      }
    };
    if digest.to_string() == EMPTY_LAYER {
      imported.skipped += 1;
      continue;
    }
    stacked.push(diff_id);
    below = Some(diff_id);
    imported.layers.push(digest);
  }

  let image_config = config.config().clone().unwrap_or_default();
  for entry in image_config.env().clone().unwrap_or_default() {
    let (key, value) = entry.split_once('=').unwrap_or((entry.as_str(), ""));
    if key == "PATH" {
      imported.path = Some(value.to_string());
    } else if !key.is_empty() {
      imported.env.insert(key.to_string(), value.to_string());
    }
  }
  if image_config.entrypoint().as_ref().is_some_and(|e| !e.is_empty()) || image_config.cmd().as_ref().is_some_and(|c| !c.is_empty()) {
    let entrypoint = image_config.entrypoint().clone().unwrap_or_default();
    let cmd = image_config.cmd().clone().unwrap_or_default();
    // `actor-base` has no entrypoint; one under /.needy is the shim step's
    // container init, which needs no word.
    if !entrypoint.first().is_some_and(|program| program.starts_with("/.needy/")) {
      imported.warnings.push(format!(
        "its Entrypoint {entrypoint:?} and Cmd {cmd:?} are ignored: a VM actor's init is needy's, and the profile's actor.command is \
         what it runs"
      ));
    }
  }
  Ok(imported)
}

/// The image manifest of `reference` for `platform` and its digest: the
/// manifest the reference names, or the one its index names for the
/// platform.
async fn resolve(
  client: &oci_client::Client,
  auth: &oci_client::secrets::RegistryAuth,
  reference: &Reference,
  platform: &str,
) -> Result<(Digest, OciImageManifest)> {
  let name = reference.as_str();
  let invalid = |reason: String| Error::Invalid {
    reference: name.to_string(),
    reason,
  };
  const ACCEPTED: &[&str] = &[
    OCI_IMAGE_MEDIA_TYPE,
    OCI_IMAGE_INDEX_MEDIA_TYPE,
    IMAGE_MANIFEST_MEDIA_TYPE,
    IMAGE_MANIFEST_LIST_MEDIA_TYPE,
  ];
  let (bytes, digest) = pull_manifest(client, auth, &reference.reference, name, ACCEPTED).await?;
  let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| invalid(format!("its manifest: {e}")))?;
  let media_type = value.get("mediaType").and_then(serde_json::Value::as_str).unwrap_or_default();
  let is_index = media_type == OCI_IMAGE_INDEX_MEDIA_TYPE
    || media_type == IMAGE_MANIFEST_LIST_MEDIA_TYPE
    || (media_type.is_empty() && value.get("manifests").is_some());
  if !is_index {
    let manifest = serde_json::from_value(value).map_err(|e| invalid(format!("its manifest: {e}")))?;
    return Ok((digest, manifest));
  }
  let index: OciImageIndex = serde_json::from_value(value).map_err(|e| invalid(format!("its index: {e}")))?;
  let (os, architecture) = platform.split_once('/').unwrap_or(("linux", platform));
  let entry =
    index.manifests.iter().find(|entry| entry.platform.as_ref().is_some_and(|p| p.os == os && p.architecture == architecture)).ok_or_else(
      || Error::Platform {
        reference: name.to_string(),
        platform: platform.to_string(),
        found: index_platforms(&index),
      },
    )?;
  let by_digest = reference.reference.clone_with_digest(entry.digest.clone());
  let (bytes, digest) = pull_manifest(client, auth, &by_digest, name, &[OCI_IMAGE_MEDIA_TYPE, IMAGE_MANIFEST_MEDIA_TYPE]).await?;
  let manifest = serde_json::from_slice(&bytes).map_err(|e| invalid(format!("its manifest for {platform}: {e}")))?;
  Ok((digest, manifest))
}

/// The platforms an index names, as an error lists them.
fn index_platforms(index: &OciImageIndex) -> String {
  let found: Vec<String> = index
    .manifests
    .iter()
    .filter_map(|entry| entry.platform.as_ref())
    // BuildKit's attestation manifests are `unknown/unknown`.
    .filter(|p| p.os != "unknown")
    .map(|p| match &p.variant {
      Some(variant) => format!("{}/{}/{variant}", p.os, p.architecture),
      None => format!("{}/{}", p.os, p.architecture),
    })
    .collect();
  if found.is_empty() {
    "no platforms".into()
  } else {
    found.join(", ")
  }
}

/// Pulls the manifest `image` names and its digest, checked against the
/// digest the reference names, if it names one.
async fn pull_manifest(
  client: &oci_client::Client,
  auth: &oci_client::secrets::RegistryAuth,
  image: &oci_client::Reference,
  name: &str,
  accepted: &[&str],
) -> Result<(Vec<u8>, Digest)> {
  let (bytes, _) = client.pull_manifest_raw(image, auth, accepted).await.map_err(|source| Error::Registry {
    reference: name.to_string(),
    source,
  })?;
  let digest = Digest::of(&bytes);
  if let Some(named) = image.digest()
    && named != digest.to_string()
  {
    return Err(Error::Invalid {
      reference: name.to_string(),
      reason: format!("the registry answered {named} with a manifest whose digest is {digest}"),
    });
  }
  Ok((bytes.to_vec(), digest))
}

/// How a layer of `media_type` is compressed, or why it is not imported.
fn compression(media_type: &str) -> std::result::Result<Compression, String> {
  match media_type {
    IMAGE_LAYER_GZIP_MEDIA_TYPE | IMAGE_DOCKER_LAYER_GZIP_MEDIA_TYPE => Ok(Compression::Gzip),
    IMAGE_LAYER_ZSTD_MEDIA_TYPE => Ok(Compression::Zstd),
    IMAGE_LAYER_MEDIA_TYPE | IMAGE_DOCKER_LAYER_TAR_MEDIA_TYPE => Ok(Compression::None),
    // Non-distributable ("foreign") layers are not in the registry.
    other if other.contains("nondistributable") || other.contains("foreign") => {
      Err(format!("it is a non-distributable layer ({other}), which the registry does not serve"))
    }
    other => Err(format!("its media type is {other}, not a tar layer (uncompressed, gzip or zstd)")),
  }
}

/// Fails unless `diff_ids` start with `base`'s.
fn check_base(name: &str, base: &manifest::Base, diff_ids: &[String], base_label: Option<&str>) -> Result<()> {
  let mismatch = base.diff_ids.iter().zip(diff_ids).position(|(expected, found)| expected != found);
  let found = match mismatch {
    Some(at) => format!("its layer {at} is {}, and the base's is {}", diff_ids[at], base.diff_ids[at]),
    None if diff_ids.len() < base.diff_ids.len() => {
      format!("it has {} layers, and the base has {}", diff_ids.len(), base.diff_ids.len())
    }
    None => return Ok(()),
  };
  let found = match base_label {
    Some(label) if !label.is_empty() => format!("{found} (its {} label names {label})", root::BASE_NAME_LABEL),
    _ => found,
  };
  Err(Error::Base {
    reference: name.to_string(),
    needs: needs(base),
    found,
  })
}

/// The base to build `FROM`, as a `FROM` line names it: pinned by digest
/// when the release pushed it, with its tag beside it.
fn needs(base: &manifest::Base) -> String {
  match &base.digest {
    Some(digest) => format!("{}@{digest} ({})", needy_config::image::untagged(&base.image), base.image),
    None => base.image.clone(),
  }
}

/// The advice for an image of another base ABI.
fn rebuild_from(base: &manifest::Base) -> String {
  format!("rebuild it FROM {} (base ABI {})", needs(base), base::ABI)
}

/// A layer an import pulled and stored.
struct Pulled {
  digest: Digest,
  /// The builder trees ([`BUILDER_TREES`]) it has, by name, with the
  /// builder.
  trees: Vec<(String, &'static str)>,
}

/// Pulls the layer `descriptor` of `reference`, checks that it decompresses
/// to `diff_id` and leaves `/.needy` alone, and stores it.
async fn pull(
  client: &oci_client::Client,
  reference: &Reference,
  descriptor: &OciDescriptor,
  compression: Compression,
  diff_id: &Digest,
  store: &Store,
) -> Result<Pulled> {
  let name = reference.as_str().to_string();
  let staged = registry::fetch_blob(client, &reference.reference, &name, descriptor, store).await?;
  let store = store.clone();
  let expected = *diff_id;
  tokio::task::spawn_blocking(move || -> Result<Pulled> {
    let (uncompressed, actual) = registry::decompress(staged.path(), compression).map_err(io(format!("decompressing a layer of {name}")))?;
    drop(staged);
    if actual != expected {
      return Err(Error::DiffId {
        reference: name,
        expected,
        actual,
      });
    }
    let trees = check_entries(&name, &expected, uncompressed.path())?;
    let layer = std::fs::File::open(uncompressed.path()).map_err(io(uncompressed.path().display()))?;
    let digest = store.put_imported(std::io::BufReader::new(layer), &expected)?;
    Ok(Pulled { digest, trees })
  })
  .await
  .map_err(|e| Error::Io {
    context: "storing an imported layer".into(),
    source: std::io::Error::other(e.to_string()),
  })?
}

/// Reads the members of the uncompressed layer `layer` (DiffID `diff_id`)
/// and fails when one writes, removes or hides `/.needy`, or is a hard link
/// to a file outside the layer; returns the builder trees it has.
fn check_entries(name: &str, diff_id: &Digest, layer: &Path) -> Result<Vec<(String, &'static str)>> {
  let failed = io(format!("reading a layer of {name}"));
  let file = std::fs::File::open(layer).map_err(io(layer.display()))?;
  let mut archive = tar::Archive::new(std::io::BufReader::new(file));
  let mut seen: BTreeSet<String> = BTreeSet::new();
  let mut trees = Vec::new();
  let needy_path = |action: &'static str, path: &str| {
    let path = path.to_string();
    Error::NeedyPath {
      reference: name.to_string(),
      layer: *diff_id,
      action,
      hint: if path.starts_with(".needy/real") {
        " (it is what the container shim step, `RUN [\"/.needy/shim-image\"]`, writes: an image for VM actors does not run it)"
      } else {
        ""
      },
      path,
    }
  };
  for entry in archive.entries().map_err(failed)? {
    let entry = entry.map_err(io(format!("reading a layer of {name}")))?;
    let path = entry.path().map_err(io(format!("reading a layer of {name}")))?;
    let Some(path) = relative(&path) else {
      continue;
    };
    let (first, top_level) = match path.split_once('/') {
      Some((first, _)) => (first, false),
      None => (path.as_str(), true),
    };
    if first == NEEDY_DIR {
      return Err(needy_path("writes", &path));
    }
    if top_level && first == format!("{}{NEEDY_DIR}", oci_layer::archive::WHITEOUT) {
      return Err(needy_path("removes", NEEDY_DIR));
    }
    if top_level && first == oci_layer::archive::OPAQUE {
      return Err(needy_path("hides everything below it, /.needy included, with", &path));
    }
    if let Some((tree, builder)) = BUILDER_TREES.iter().find(|(tree, _)| *tree == first)
      && !trees.iter().any(|(found, _): &(String, &str)| found == tree)
    {
      trees.push((tree.to_string(), *builder));
    }
    if entry.header().entry_type() == tar::EntryType::Link {
      let target = entry.link_name().map_err(io(format!("reading a layer of {name}")))?.and_then(|target| relative(&target));
      if !target.as_ref().is_some_and(|target| seen.contains(target)) {
        // TODO(import-hard-links): a hard link to a file of a lower layer
        // has no file to link to in this layer's disk. Builders write hard
        // links within one layer only (BuildKit and moby diff a layer by
        // inode); reopen when an image needs one across layers.
        return Err(Error::Invalid {
          reference: name.to_string(),
          reason: format!(
            "layer {diff_id} has a hard link /{path} to {}, which is not a file of the same layer",
            target.map_or_else(|| "nothing".to_string(), |t| format!("/{t}"))
          ),
        });
      }
    }
    seen.insert(path);
  }
  Ok(trees)
}

/// `path` relative to the layer's root, as the store's canonical form names
/// it (no leading `/` or `./`), or `None` for the root itself or a path that
/// climbs out of the layer, which the store refuses on its own. Parts are
/// joined with `/` whatever the host: a layer's paths are the guest's, and
/// a Windows `PathBuf` would print them with `\`.
fn relative(path: &Path) -> Option<String> {
  let mut parts = Vec::new();
  for component in path.components() {
    match component {
      Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
      Component::RootDir | Component::CurDir => {}
      Component::ParentDir | Component::Prefix(_) => return None,
    }
  }
  (!parts.is_empty()).then(|| parts.join("/"))
}
