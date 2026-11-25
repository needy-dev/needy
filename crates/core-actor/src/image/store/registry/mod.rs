//! A layer registry: the store's built steps as ordinary OCI images in one
//! repository of an OCI Distribution registry, so that hosts sharing it
//! build each step once.
//!
//! One built step is one image, tagged `key-<cache key hex>`: an OCI image
//! manifest whose single layer is the step's canonical layer compressed
//! with gzip, and whose config is a standard image config (`architecture`,
//! `os`, `rootfs.type = layers`, `rootfs.diff_ids` naming that layer).
//! needy's own metadata (the cache key, the layer it was built on, the
//! root it was built on, the feature it installs) is in the manifest's
//! annotations, under `dev.needy.image.`. Any registry and any OCI tool
//! (`crane`, `skopeo`, `oras`, scanners) can read, copy and keep it.
//!
//! The registry verifies the compressed digest of what is pushed;
//! [`pull_layer`] verifies the DiffID of what it decompresses, and the
//! store verifies it again before it keeps the layer. Layers are compressed
//! for the registry only: the store keeps the uncompressed canonical archive.
//!
//! Registry access (credentials, `certs.d`, insecure registries, the
//! identity-token exchange) is [`oci_registry::Access`]'s.
//!
//! Unlike the rest of the store this module is async (registry IO); its
//! file IO runs off the async runtime.
//!
//! See `docs/ai/references/architecture/2026-11-25-layered-oci-base-spec.md`
//! (B5, "Settled in B-3").
// Source: https://github.com/opencontainers/distribution-spec/blob/v1.1.0/spec.md
// Reference: https://github.com/opencontainers/image-spec/blob/v1.1.0/manifest.md
// and https://github.com/opencontainers/image-spec/blob/v1.1.0/config.md
// (`rootfs.diff_ids`: "the uncompressed layer content" digests, lowest first).
// NOTICE: a step image holds the step's layer only, not the base below it,
// so it is a valid image but not a runnable one: what it was built on is
// named by its annotations. A full image `FROM actor-base` needs the base's
// blobs in this repository; see the spec's "Settled in B-3".
// TODO(snapshot-image): pushing a snapshot as a full image `FROM
// actor-base` (mounting or copying the base's blobs into the repository) is
// not done; B-4 imports full images but pushes none. Reopen with B-5's
// `needy image build --push`, the first command that pushes one.
// TODO(layer-registry-sharing): one repository per tenant; sharing layers
// across tenants waits for an owner decision (remote actor images spec D5).
// TODO(layer-registry): images are never deleted by needy; their lifecycle
// (by last pull, or kept while a lockfile names them) is the remote actor
// images spec's open question 2.

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use oci_client::errors::{OciDistributionError, OciErrorCode};
use oci_client::manifest::{IMAGE_CONFIG_MEDIA_TYPE, IMAGE_LAYER_GZIP_MEDIA_TYPE, OCI_IMAGE_MEDIA_TYPE, OciDescriptor, OciImageManifest};
use oci_registry::{Access, Operation};
use oci_spec::image::{Arch, ImageConfiguration, ImageConfigurationBuilder, Os, RootFsBuilder};
use sha2::Digest as _;

use super::{Digest, Store};

/// The annotation keys of a step image's manifest.
// NOTICE: needy's own keys, not registered ones. They follow the OCI
// annotation rules (image-spec v1.1.0 `annotations.md`, "Rules": reverse
// domain notation, `org.opencontainers` reserved, string values) under
// `dev.needy.`, the reverse-DNS prefix needy already uses for its service
// label (`dev.needy.control`). Readers ignore keys they do not know;
// renaming one orphans every pushed step.
// Source: https://github.com/opencontainers/image-spec/blob/v1.1.0/annotations.md#rules
pub mod annotation {
  /// The step's cache key (`sha256:<hex>`).
  pub const KEY: &str = "dev.needy.image.step.key";
  /// The layer the step was built on (`sha256:<hex>` DiffID); absent for a
  /// step on the root.
  pub const PARENT: &str = "dev.needy.image.step.parent";
  /// The root the step was built on: the base's stack digest (B-2).
  pub const ROOT: &str = "dev.needy.image.root";
  /// The Dev Container feature the step installed, as the image definition
  /// wrote it.
  pub const FEATURE_REF: &str = "dev.needy.image.feature.ref";
  /// The digest of that feature's manifest.
  pub const FEATURE_DIGEST: &str = "dev.needy.image.feature.digest";
}

/// What a layer registry refused or could not do.
#[derive(Debug, thiserror::Error)]
pub enum Error {
  /// The configured repository is not a registry repository.
  #[error("{text:?} is not a layer registry repository (`[http://]HOST[:PORT]/PATH`): {reason}")]
  Repository { text: String, reason: String },
  /// The registry client or its credentials could not be set up.
  #[error("layer registry {reference}: {source}")]
  Access {
    reference: String,
    #[source]
    source: oci_registry::Error,
  },
  /// The registry refused a request or could not be reached.
  #[error("layer registry {reference}: {source}")]
  Registry {
    reference: String,
    #[source]
    source: OciDistributionError,
  },
  /// What the registry holds under a step's tag is not a needy step image.
  #[error("layer registry {reference} is not a needy step image: {reason}")]
  Invalid { reference: String, reason: String },
  /// A pulled layer decompressed to another layer than its config names.
  #[error("layer registry {reference} holds the layer {actual}, not the {expected} its config names")]
  DiffId {
    reference: String,
    expected: Digest,
    actual: Digest,
  },
  /// The local store could not read or take the layer.
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

type Result<T> = std::result::Result<T, Error>;

fn io(context: impl fmt::Display) -> impl FnOnce(std::io::Error) -> Error {
  move |source| Error::Io {
    context: context.to_string(),
    source,
  }
}

/// One repository of an OCI Distribution registry that holds steps, as an
/// operator names it: `registry.example.com/needy/layers`, over HTTPS, or
/// `http://registry.example.com:5000/needy/layers`, over plain HTTP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
  registry: String,
  path: String,
  plain_http: bool,
}

impl Repository {
  /// Parses an operator's repository. The registry host is explicit (no
  /// Docker Hub default), and a repository names no tag or digest.
  pub fn parse(text: &str) -> Result<Self> {
    let bad = |reason: &str| Error::Repository {
      text: text.to_string(),
      reason: reason.to_string(),
    };
    let (plain_http, rest) = if let Some(rest) = text.strip_prefix("http://") {
      (true, rest)
    } else if let Some(rest) = text.strip_prefix("https://") {
      (false, rest)
    } else if text.contains("://") {
      return Err(bad("the only schemes are `http://` and `https://`"));
    } else {
      (false, text)
    };
    let rest = rest.trim_end_matches('/');
    let Some((registry, path)) = rest.split_once('/') else {
      return Err(bad("it names no repository path after the registry host"));
    };
    if !(registry.contains('.') || registry.contains(':') || registry == "localhost") {
      return Err(bad("the registry host must contain `.` or `:`, or be `localhost`"));
    }
    // The OCI reference grammar (`oci-spec`'s parser) checks the host and
    // the repository's path components; a probe tag proves that the text
    // itself carries none.
    let probe = format!("{rest}:probe");
    let reference = oci_client::Reference::from_str(&probe).map_err(|e| bad(&e.to_string()))?;
    if reference.registry() != registry || reference.repository() != path || reference.tag() != Some("probe") || reference.digest().is_some()
    {
      return Err(bad("a repository names no tag or digest"));
    }
    Ok(Self {
      registry: registry.to_string(),
      path: path.to_string(),
      plain_http,
    })
  }

  /// The registry host, with its port if any.
  pub fn registry(&self) -> &str {
    &self.registry
  }

  /// The repository's path in the registry.
  pub fn path(&self) -> &str {
    &self.path
  }

  /// Whether the registry is reached over plain HTTP.
  pub fn plain_http(&self) -> bool {
    self.plain_http
  }

  /// The tag of the step built under the cache key `key`: `key-<hex>`.
  pub fn tag(key: &Digest) -> String {
    format!("key-{}", key.hex())
  }

  /// The reference of the step built under the cache key `key`.
  pub fn reference(&self, key: &Digest) -> String {
    format!("{}/{}:{}", self.registry, self.path, Self::tag(key))
  }

  fn tagged(&self, key: &Digest) -> oci_client::Reference {
    oci_client::Reference::with_tag(self.registry.clone(), self.path.clone(), Self::tag(key))
  }
}

impl FromStr for Repository {
  type Err = Error;

  fn from_str(text: &str) -> Result<Self> {
    Self::parse(text)
  }
}

impl fmt::Display for Repository {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    if self.plain_http {
      f.write_str("http://")?;
    }
    write!(f, "{}/{}", self.registry, self.path)
  }
}

/// A built step as its image describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
  /// The digest of the uncompressed canonical layer archive (its OCI
  /// DiffID): the layer's name in every store.
  pub diff_id: Digest,
  /// The cache key the step was built under.
  pub key: Digest,
  /// The layer the step was built on, `None` for a step on the root.
  pub parent: Option<Digest>,
  /// The root the step was built on (the base's stack digest).
  pub root: Digest,
  /// The Dev Container feature the step installed, if it installed one.
  pub feature: Option<Feature>,
}

/// A Dev Container feature as a step image names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Feature {
  /// The reference as the image definition wrote it.
  pub reference: String,
  /// The digest of the feature artifact's manifest.
  pub digest: Digest,
}

impl Step {
  fn annotations(&self) -> BTreeMap<String, String> {
    let mut annotations = BTreeMap::from([
      (annotation::KEY.to_string(), self.key.to_string()),
      (annotation::ROOT.to_string(), self.root.to_string()),
    ]);
    if let Some(parent) = &self.parent {
      annotations.insert(annotation::PARENT.to_string(), parent.to_string());
    }
    if let Some(feature) = &self.feature {
      annotations.insert(annotation::FEATURE_REF.to_string(), feature.reference.clone());
      annotations.insert(annotation::FEATURE_DIGEST.to_string(), feature.digest.to_string());
    }
    annotations
  }

  fn from_image(annotations: &BTreeMap<String, String>, diff_id: Digest) -> std::result::Result<Self, String> {
    let digest = |key: &str| -> std::result::Result<Option<Digest>, String> {
      annotations.get(key).map(|text| Digest::parse(text).map_err(|e| format!("annotation {key} {text:?}: {e}"))).transpose()
    };
    let required = |key: &str| digest(key)?.ok_or_else(|| format!("it has no annotation {key}"));
    let feature = match (annotations.get(annotation::FEATURE_REF), digest(annotation::FEATURE_DIGEST)?) {
      (Some(reference), Some(digest)) => Some(Feature {
        reference: reference.clone(),
        digest,
      }),
      (None, None) => None,
      _ => return Err(format!("it has only one of {} and {}", annotation::FEATURE_REF, annotation::FEATURE_DIGEST)),
    };
    Ok(Self {
      diff_id,
      key: required(annotation::KEY)?,
      parent: digest(annotation::PARENT)?,
      root: required(annotation::ROOT)?,
      feature,
    })
  }
}

/// The standard image config of an image with the single layer `diff_id`,
/// for this host's architecture (an actor's platform is its host's).
fn image_config(diff_id: &Digest) -> std::result::Result<ImageConfiguration, String> {
  let rootfs = RootFsBuilder::default().typ("layers").diff_ids(vec![diff_id.to_string()]).build().map_err(|e| e.to_string())?;
  ImageConfigurationBuilder::default().architecture(Arch::default()).os(Os::Linux).rootfs(rootfs).build().map_err(|e| e.to_string())
}

/// A step the registry holds for a cache key, as [`Registry::lookup`] found
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteStep {
  /// Where it was found: `registry/path:key-<hex>`.
  pub reference: String,
  /// The digest of its manifest.
  pub manifest: Digest,
  /// What its manifest and config say.
  pub step: Step,
  /// The digest of its gzip layer blob.
  pub blob: Digest,
  /// The size of its gzip layer blob in bytes.
  pub size: u64,
}

/// What [`Registry::push`] pushed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pushed {
  /// The tag's reference: `registry/path:key-<hex>`.
  pub reference: String,
  /// The digest of the pushed manifest.
  pub manifest: Digest,
  /// The digest of the compressed blob.
  pub blob: Digest,
  /// The size of the compressed blob in bytes.
  pub size: u64,
}

/// A client of one step repository. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Registry {
  repository: Repository,
  access: Access,
}

impl Registry {
  pub fn new(repository: Repository, access: Access) -> Self {
    Self { repository, access }
  }

  pub fn repository(&self) -> &Repository {
    &self.repository
  }

  /// The step the registry holds for the cache key `key`, or `None` when it
  /// holds none. Reads the tag's manifest and the config it names.
  pub async fn lookup(&self, key: &Digest) -> Result<Option<RemoteStep>> {
    let reference = self.repository.reference(key);
    let tagged = self.repository.tagged(key);
    let connection = self.connect(&reference, Operation::Pull).await?;
    let registry_error = |source| Error::Registry {
      reference: reference.clone(),
      source,
    };
    let invalid = |reason: String| Error::Invalid {
      reference: reference.clone(),
      reason,
    };

    let (manifest_bytes, manifest_digest) =
      match connection.client.pull_manifest_raw(&tagged, &connection.auth, &[OCI_IMAGE_MEDIA_TYPE]).await {
        Ok(found) => found,
        Err(e) if not_found(&e) => return Ok(None),
        Err(e) => return Err(registry_error(e)),
      };
    let manifest_digest = Digest::parse(&manifest_digest).map_err(|e| invalid(format!("manifest digest {manifest_digest}: {e}")))?;
    let manifest: OciImageManifest = serde_json::from_slice(&manifest_bytes).map_err(|e| invalid(format!("manifest: {e}")))?;
    if manifest.config.media_type != IMAGE_CONFIG_MEDIA_TYPE {
      return Err(invalid(format!("its config is {:?}, not {IMAGE_CONFIG_MEDIA_TYPE:?}", manifest.config.media_type)));
    }
    let [layer] = manifest.layers.as_slice() else {
      return Err(invalid(format!("it has {} layers, not one", manifest.layers.len())));
    };
    // TODO(layer-registry-zstd): zstd (`+zstd`) and uncompressed layer
    // blobs; needy pushes gzip only, and B-4's import of images built by
    // other tools is where other compressions first appear.
    if layer.media_type != IMAGE_LAYER_GZIP_MEDIA_TYPE {
      return Err(invalid(format!("its layer is {:?}, not {IMAGE_LAYER_GZIP_MEDIA_TYPE:?}", layer.media_type)));
    }

    // NOTICE: `pull_blob` checks what it reads against the descriptor's
    // digest, so a config that does not match its manifest is refused.
    let mut config = Vec::new();
    connection.client.pull_blob(&tagged, &manifest.config, &mut config).await.map_err(registry_error)?;
    let config: ImageConfiguration = serde_json::from_slice(&config).map_err(|e| invalid(format!("config: {e}")))?;
    let [diff_id] = config.rootfs().diff_ids().as_slice() else {
      return Err(invalid(format!("its config names {} layers, not one", config.rootfs().diff_ids().len())));
    };
    let diff_id = Digest::parse(diff_id).map_err(|e| invalid(format!("DiffID {diff_id}: {e}")))?;
    let step = Step::from_image(&manifest.annotations.unwrap_or_default(), diff_id).map_err(invalid)?;
    if step.key != *key {
      return Err(invalid(format!("its annotations name the cache key {}", step.key)));
    }
    let blob = Digest::parse(&layer.digest).map_err(|e| invalid(format!("layer digest {}: {e}", layer.digest)))?;
    let size = u64::try_from(layer.size).map_err(|_| invalid(format!("layer size {}", layer.size)))?;
    Ok(Some(RemoteStep {
      reference,
      manifest: manifest_digest,
      step,
      blob,
      size,
    }))
  }

  /// Pulls `remote`'s layer into `store` and returns its digest, which is
  /// the DiffID its config names: a blob that decompresses to another layer
  /// is refused and not kept.
  pub async fn pull(&self, store: &Store, remote: &RemoteStep) -> Result<Digest> {
    let tagged = self.repository.tagged(&remote.step.key);
    let connection = self.connect(&remote.reference, Operation::Pull).await?;
    let descriptor = OciDescriptor {
      media_type: IMAGE_LAYER_GZIP_MEDIA_TYPE.to_string(),
      digest: remote.blob.to_string(),
      size: i64::try_from(remote.size).unwrap_or(i64::MAX),
      ..Default::default()
    };
    pull_layer(&connection.client, &tagged, &remote.reference, &descriptor, &remote.step.diff_id, store).await
  }

  /// Pushes the stored layer `step.diff_id` as the image of the cache key
  /// `step.key`: its gzip blob, a standard config naming it, and the
  /// manifest with `step`'s annotations under the key's tag, replacing
  /// whatever the tag named.
  // NOTICE: a later push of the same key replaces the tag (last writer
  // wins). Builds are not reproducible, so two hosts racing on one key may
  // push different layers; either is a valid build of the key, and blobs
  // stay pullable by digest.
  pub async fn push(&self, store: &Store, step: &Step) -> Result<Pushed> {
    let reference = self.repository.reference(&step.key);
    let tagged = self.repository.tagged(&step.key);
    let registry_error = |source| Error::Registry {
      reference: reference.clone(),
      source,
    };
    let encoding = |context: String| {
      move |e: String| Error::Io {
        context,
        source: std::io::Error::other(e),
      }
    };

    let layer = store.open_layer(&step.diff_id)?;
    let diff_id = step.diff_id;
    // TODO(layer-registry): the compressed layer is held in memory because
    // `oci-client` 0.15 pushes a blob from a slice; `push_blob_stream` (0.16
    // and later) streams it. Reopen when the workspace upgrades `oci-client`.
    let (blob_bytes, blob) = tokio::task::spawn_blocking(move || -> Result<(Vec<u8>, Digest)> {
      let context = format!("compressing layer {diff_id}");
      let mut layer = std::io::BufReader::new(layer);
      let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
      std::io::copy(&mut layer, &mut encoder).map_err(io(&context))?;
      let bytes = encoder.finish().map_err(io(&context))?;
      let digest = Digest::from_bytes(sha2::Sha256::digest(&bytes).into());
      Ok((bytes, digest))
    })
    .await
    .map_err(|e| Error::Io {
      context: format!("compressing layer {}", step.diff_id),
      source: std::io::Error::other(e.to_string()),
    })??;
    let config = image_config(&step.diff_id).map_err(encoding(format!("the config of layer {}", step.diff_id)))?;
    let config_bytes =
      serde_json::to_vec(&config).map_err(|e| e.to_string()).map_err(encoding(format!("the config of layer {}", step.diff_id)))?;
    let config_digest = Digest::of(&config_bytes);

    let connection = self.connect(&reference, Operation::Push).await?;
    let client = &connection.client;
    client.push_blob(&tagged, &config_bytes, &config_digest.to_string()).await.map_err(registry_error)?;
    client.push_blob(&tagged, &blob_bytes, &blob.to_string()).await.map_err(registry_error)?;
    let size = blob_bytes.len() as u64;
    let manifest = OciImageManifest {
      schema_version: 2,
      media_type: Some(OCI_IMAGE_MEDIA_TYPE.to_string()),
      config: OciDescriptor {
        media_type: IMAGE_CONFIG_MEDIA_TYPE.to_string(),
        digest: config_digest.to_string(),
        size: config_bytes.len() as i64,
        ..Default::default()
      },
      layers: vec![OciDescriptor {
        media_type: IMAGE_LAYER_GZIP_MEDIA_TYPE.to_string(),
        digest: blob.to_string(),
        size: size as i64,
        ..Default::default()
      }],
      annotations: Some(step.annotations()),
      ..Default::default()
    };
    // The manifest is pushed as these bytes, so their digest is the
    // manifest's whatever URL the registry answers with.
    let manifest_bytes =
      serde_json::to_vec(&manifest).map_err(|e| e.to_string()).map_err(encoding(format!("the manifest of {reference}")))?;
    let manifest_digest = Digest::of(&manifest_bytes);
    let content_type = hyper::header::HeaderValue::from_static(OCI_IMAGE_MEDIA_TYPE);
    client.push_manifest_raw(&tagged, manifest_bytes, content_type).await.map_err(registry_error)?;
    Ok(Pushed {
      reference,
      manifest: manifest_digest,
      blob,
      size,
    })
  }

  async fn connect(&self, reference: &str, operation: Operation) -> Result<oci_registry::Connection> {
    self.access.connect(&self.repository.registry, self.repository.plain_http, &self.repository.path, operation).await.map_err(|source| {
      Error::Access {
        reference: reference.to_string(),
        source,
      }
    })
  }
}

/// Pulls the gzip layer `descriptor` of the image `image` (`reference` as
/// errors name it) into `store`, only if it decompresses to `diff_id`: the
/// registry's blob is checked against the descriptor's digest as it is read,
/// the decompressed stream against `diff_id`, and the store keeps the layer
/// only when its canonical form is `diff_id` too. Nothing is kept otherwise.
// NOTICE: a step image's layer is needy's own canonical archive, so its
// canonical form is its DiffID. Layers of images other tools built are
// imported by `super::import`, which names them by their canonical digest.
pub async fn pull_layer(
  client: &oci_client::Client,
  image: &oci_client::Reference,
  reference: &str,
  descriptor: &OciDescriptor,
  diff_id: &Digest,
  store: &Store,
) -> Result<Digest> {
  let staged = fetch_blob(client, image, reference, descriptor, store).await?;
  let store = store.clone();
  let reference = reference.to_string();
  let expected = *diff_id;
  blocking("storing a pulled layer", move || -> Result<Digest> {
    let diff_id_mismatch = |actual| Error::DiffId {
      reference: reference.clone(),
      expected,
      actual,
    };
    let (uncompressed, actual) = decompress(staged.path(), Compression::Gzip).map_err(io(format!("decompressing {reference}")))?;
    if actual != expected {
      return Err(diff_id_mismatch(actual));
    }
    let layer = std::fs::File::open(uncompressed.path()).map_err(io(uncompressed.path().display()))?;
    store.put_expecting(std::io::BufReader::new(layer), &expected).map_err(|e| match e {
      super::Error::Unexpected { actual, .. } => diff_id_mismatch(actual),
      e => Error::Store(e),
    })
  })
  .await
}

/// Runs the blocking `work` off the async runtime.
pub(super) async fn blocking<T: Send + 'static>(what: &str, work: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
  tokio::task::spawn_blocking(work).await.map_err(|e| Error::Io {
    context: what.to_string(),
    source: std::io::Error::other(e.to_string()),
  })?
}

/// Pulls the blob `descriptor` of the image `image` into a new file in the
/// store's staging directory. `oci-client`'s `pull_blob` checks what it
/// reads against the descriptor's digest.
pub(super) async fn fetch_blob(
  client: &oci_client::Client,
  image: &oci_client::Reference,
  reference: &str,
  descriptor: &OciDescriptor,
  store: &Store,
) -> Result<tempfile::NamedTempFile> {
  let staging = store.cache.join("registry");
  tokio::fs::create_dir_all(&staging).await.map_err(io(staging.display()))?;
  let staged = tempfile::NamedTempFile::new_in(&staging).map_err(io(staging.display()))?;
  let mut file = tokio::fs::File::from_std(staged.as_file().try_clone().map_err(io(staged.path().display()))?);
  // NOTICE: `pull_blob` verifies the compressed digest as it reads.
  client.pull_blob(image, descriptor, &mut file).await.map_err(|source| Error::Registry {
    reference: reference.to_string(),
    source,
  })?;
  file.sync_all().await.map_err(io(staged.path().display()))?;
  Ok(staged)
}

/// How a layer blob is compressed, by its media type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Compression {
  None,
  Gzip,
  Zstd,
}

/// Decompresses the blob at `compressed` into a new file beside it and
/// returns the file and the digest of what it decompressed to: the layer's
/// DiffID, which image-spec `config.md` defines on the decompressed stream
/// as the registry serves it. Blocking.
pub(super) fn decompress(compressed: &std::path::Path, compression: Compression) -> std::io::Result<(tempfile::NamedTempFile, Digest)> {
  let dir = compressed.parent().unwrap_or(compressed);
  let uncompressed = tempfile::NamedTempFile::new_in(dir)?;
  let blob = std::io::BufReader::new(std::fs::File::open(compressed)?);
  let mut reader: Box<dyn std::io::Read> = match compression {
    Compression::None => Box::new(blob),
    Compression::Gzip => Box::new(flate2::read::GzDecoder::new(blob)),
    Compression::Zstd => Box::new(Zstd::new(blob)),
  };
  let mut out = Hashing {
    inner: std::io::BufWriter::new(uncompressed.as_file().try_clone()?),
    hasher: sha2::Sha256::new(),
  };
  std::io::copy(&mut reader, &mut out)?;
  let Hashing { mut inner, hasher } = out;
  std::io::Write::flush(&mut inner)?;
  drop(inner);
  Ok((uncompressed, Digest::from_bytes(hasher.finalize().into())))
}

/// A reader of a whole zstd stream: every frame in turn, skippable frames
/// skipped, as `zstd -d` reads one. `ruzstd`'s `StreamingDecoder` reads one
/// frame only, and zstd:chunked layers (Podman, Buildah) are many frames
/// with skippable frames between them.
// Reference: RFC 8878, section 3.1 ("Frames": "Zstandard compressed data is
// made of one or more frames") and 3.1.2 ("Skippable Frames"),
// https://www.rfc-editor.org/rfc/rfc8878#section-3.1
// NOTICE: hand-written around `ruzstd::decoding::FrameDecoder`, following
// `ruzstd` 0.8.3 `src/decoding/streaming_decoder.rs` (`Read for
// StreamingDecoder`), because that type owns its source and so cannot go on
// to the next frame.
struct Zstd<R> {
  source: R,
  frame: ruzstd::decoding::FrameDecoder,
  started: bool,
}

impl<R: std::io::BufRead> Zstd<R> {
  fn new(source: R) -> Self {
    Self {
      source,
      frame: ruzstd::decoding::FrameDecoder::new(),
      started: false,
    }
  }

  /// Starts the next frame, skipping skippable ones; `false` at the end of
  /// the stream.
  fn next_frame(&mut self) -> std::io::Result<bool> {
    use ruzstd::decoding::errors::{FrameDecoderError, ReadFrameHeaderError};
    loop {
      if self.source.fill_buf()?.is_empty() {
        return Ok(false);
      }
      match self.frame.reset(&mut self.source) {
        Ok(()) => return Ok(true),
        Err(FrameDecoderError::ReadFrameHeaderError(ReadFrameHeaderError::SkipFrame { length, .. })) => {
          let skipped = std::io::copy(&mut std::io::Read::take(&mut self.source, u64::from(length)), &mut std::io::sink())?;
          if skipped != u64::from(length) {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "a zstd skippable frame ends early"));
          }
        }
        Err(e) => return Err(std::io::Error::other(e)),
      }
    }
  }
}

impl<R: std::io::BufRead> std::io::Read for Zstd<R> {
  fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
    use ruzstd::decoding::BlockDecodingStrategy;
    if buf.is_empty() {
      return Ok(0);
    }
    loop {
      if !self.started || (self.frame.is_finished() && self.frame.can_collect() == 0) {
        if !self.next_frame()? {
          return Ok(0);
        }
        self.started = true;
      }
      while self.frame.can_collect() < buf.len() && !self.frame.is_finished() {
        let wanted = buf.len() - self.frame.can_collect();
        self.frame.decode_blocks(&mut self.source, BlockDecodingStrategy::UptoBytes(wanted)).map_err(std::io::Error::other)?;
      }
      let read = self.frame.read(buf)?;
      if read > 0 {
        return Ok(read);
      }
    }
  }
}

/// A writer that hashes what it writes.
struct Hashing<W> {
  inner: W,
  hasher: sha2::Sha256,
}

impl<W: std::io::Write> std::io::Write for Hashing<W> {
  fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
    let written = self.inner.write(buf)?;
    self.hasher.update(&buf[..written]);
    Ok(written)
  }

  fn flush(&mut self) -> std::io::Result<()> {
    self.inner.flush()
  }
}

/// Whether a registry answered that it has no manifest under a tag.
// Source: https://github.com/opencontainers/distribution-spec/blob/v1.1.0/spec.md#pulling-manifests
// ("If the manifest is not found in the repository, the response code MUST
// be `404 Not Found`"); `MANIFEST_UNKNOWN` and `NAME_UNKNOWN` from its
// error code table, https://github.com/opencontainers/distribution-spec/blob/v1.1.0/spec.md#error-codes
fn not_found(error: &OciDistributionError) -> bool {
  match error {
    OciDistributionError::ImageManifestNotFoundError(_) => true,
    OciDistributionError::ServerError { code, .. } => *code == 404,
    OciDistributionError::RegistryError { envelope, .. } => {
      envelope.errors.iter().all(|e| matches!(e.code, OciErrorCode::ManifestUnknown | OciErrorCode::NameUnknown))
    }
    _ => false,
  }
}
