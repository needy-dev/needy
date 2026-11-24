//! A layer registry: the store's built layers as OCI artifacts in one
//! repository of an OCI Distribution registry, so that hosts sharing it
//! build each step once.
//!
//! One built step is one artifact, tagged `key-<cache key hex>`: an OCI
//! image manifest whose single layer is the step's canonical layer
//! compressed with gzip, and whose config is needy's [`LayerConfig`] (the
//! layer's DiffID, its cache key, the layer it was built on, and the
//! feature it installs). The registry verifies the compressed digest of
//! what is pushed; [`Registry::pull`] verifies the DiffID of what it
//! decompresses into the store. Layers are compressed for the registry
//! only: the store keeps the uncompressed canonical archive.
//!
//! Unlike the rest of the store this module is async (registry IO); its
//! file IO runs off the async runtime.
//!
//! See `docs/ai/references/architecture/2026-11-25-remote-actor-images-spec.md`
//! (D5).
// Source: https://github.com/opencontainers/distribution-spec/blob/v1.1.0/spec.md
// Reference: https://github.com/opencontainers/image-spec/blob/v1.1.0/manifest.md
// (an artifact is an image manifest whose `config.mediaType` names it).
// TODO(layer-registry-sharing): one repository per tenant; sharing layers
// across tenants waits for an owner decision (spec D5).
// TODO(layer-registry): artifacts are never deleted by needy; their lifecycle
// (by last pull, or kept while a lockfile names them) is the spec's open
// question 2.

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

use std::fmt;
use std::str::FromStr;

use oci_client::client::{ClientConfig, ClientProtocol};
use oci_client::errors::{OciDistributionError, OciErrorCode};
use oci_client::manifest::{IMAGE_LAYER_GZIP_MEDIA_TYPE, OCI_IMAGE_MEDIA_TYPE, OciDescriptor, OciImageManifest, OciManifest};
use oci_client::secrets::RegistryAuth;
use sha2::Digest as _;

use super::{Digest, Store};

/// The media type of a layer artifact's config blob, a [`LayerConfig`].
// NOTICE: needy's own media type (provisional, spec D5), not a registered
// one; it follows the OCI image-spec v1.1 artifact guidance
// (`https://github.com/opencontainers/image-spec/blob/v1.1.0/manifest.md#guidelines-for-artifact-usage`)
// and the RFC 6838 vendor tree (`vnd.`). Changing it orphans every pushed
// artifact: readers refuse another config media type.
pub const CONFIG_MEDIA_TYPE: &str = "application/vnd.needy.layer.config.v1+json";

/// What a layer registry refused or could not do.
#[derive(Debug, thiserror::Error)]
pub enum Error {
  /// The configured repository is not a registry repository.
  #[error("{text:?} is not a layer registry repository (`[http://]HOST[:PORT]/PATH`): {reason}")]
  Repository { text: String, reason: String },
  /// The registry refused a request or could not be reached.
  #[error("layer registry {reference}: {source}")]
  Registry {
    reference: String,
    #[source]
    source: OciDistributionError,
  },
  /// What the registry holds under a layer's tag is not a needy layer
  /// artifact.
  #[error("layer registry {reference} is not a needy layer: {reason}")]
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

/// One repository of an OCI Distribution registry that holds layers, as an
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

  /// The tag of the layer built under the cache key `key`: `key-<hex>`.
  pub fn tag(key: &Digest) -> String {
    format!("key-{}", key.hex())
  }

  /// The reference of the layer built under the cache key `key`.
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

/// A layer artifact's config: what the layer is and what made it.
// NOTICE: provisional shape (spec D5). Readers ignore fields they do not
// know, so a later writer may add optional ones without a new media type.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LayerConfig {
  /// The digest of the uncompressed canonical layer archive (its OCI
  /// DiffID): the layer's name in every store.
  pub diff_id: Digest,
  /// The cache key the layer was built under.
  pub key: Digest,
  /// The layer the step was built on, `None` for a step on the root.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub parent: Option<Digest>,
  /// The Dev Container feature the step installed, if it installed one.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub feature: Option<Feature>,
}

/// A Dev Container feature as a layer artifact's config names it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Feature {
  /// The reference as the image definition wrote it.
  pub reference: String,
  /// The digest of the feature artifact's manifest.
  pub digest: Digest,
}

/// A layer the registry holds for a cache key, as [`Registry::lookup`]
/// found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteLayer {
  /// Where it was found: `registry/path:key-<hex>`.
  pub reference: String,
  /// The digest of its manifest.
  pub manifest: Digest,
  /// Its config.
  pub config: LayerConfig,
  /// The digest of its compressed blob.
  pub blob: Digest,
  /// The size of its compressed blob in bytes.
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

/// A client of one layer repository. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Registry {
  repository: Repository,
}

impl Registry {
  pub fn new(repository: Repository) -> Self {
    Self { repository }
  }

  pub fn repository(&self) -> &Repository {
    &self.repository
  }

  /// The layer the registry holds for the cache key `key`, or `None` when
  /// it holds none. Reads the tag's manifest and the config it names.
  pub async fn lookup(&self, key: &Digest) -> Result<Option<RemoteLayer>> {
    let reference = self.repository.reference(key);
    let tagged = self.repository.tagged(key);
    let client = self.client(&tagged).await;
    let registry_error = |source| Error::Registry {
      reference: reference.clone(),
      source,
    };
    let invalid = |reason: String| Error::Invalid {
      reference: reference.clone(),
      reason,
    };

    let (manifest_bytes, manifest_digest) = match client.pull_manifest_raw(&tagged, &RegistryAuth::Anonymous, &[OCI_IMAGE_MEDIA_TYPE]).await
    {
      Ok(found) => found,
      Err(e) if not_found(&e) => return Ok(None),
      Err(e) => return Err(registry_error(e)),
    };
    let manifest_digest = Digest::parse(&manifest_digest).map_err(|e| invalid(format!("manifest digest {manifest_digest}: {e}")))?;
    let manifest: OciImageManifest = serde_json::from_slice(&manifest_bytes).map_err(|e| invalid(format!("manifest: {e}")))?;
    if manifest.config.media_type != CONFIG_MEDIA_TYPE {
      return Err(invalid(format!("its config is {:?}, not {CONFIG_MEDIA_TYPE:?}", manifest.config.media_type)));
    }
    // TODO(layer-registry): zstd or uncompressed layer blobs; needy pushes
    // gzip only, so nothing else is read until a writer produces it.
    let [layer] = manifest.layers.as_slice() else {
      return Err(invalid(format!("it has {} layers, not one", manifest.layers.len())));
    };
    if layer.media_type != IMAGE_LAYER_GZIP_MEDIA_TYPE {
      return Err(invalid(format!("its layer is {:?}, not {IMAGE_LAYER_GZIP_MEDIA_TYPE:?}", layer.media_type)));
    }
    let blob = Digest::parse(&layer.digest).map_err(|e| invalid(format!("layer digest {}: {e}", layer.digest)))?;
    let size = u64::try_from(layer.size).map_err(|_| invalid(format!("layer size {}", layer.size)))?;

    // NOTICE: `pull_blob` checks what it reads against the descriptor's
    // digest, so a config that does not match its manifest is refused.
    let mut config = Vec::new();
    client.pull_blob(&tagged, &manifest.config, &mut config).await.map_err(registry_error)?;
    let config: LayerConfig = serde_json::from_slice(&config).map_err(|e| invalid(format!("config: {e}")))?;
    if config.key != *key {
      return Err(invalid(format!("its config names the cache key {}", config.key)));
    }
    Ok(Some(RemoteLayer {
      reference,
      manifest: manifest_digest,
      config,
      blob,
      size,
    }))
  }

  /// Pulls `layer` into `store` and returns its digest, which is the DiffID
  /// its config names: a blob that decompresses to another layer is
  /// refused and not kept.
  pub async fn pull(&self, store: &Store, layer: &RemoteLayer) -> Result<Digest> {
    let tagged = self.repository.tagged(&layer.config.key);
    let client = self.client(&tagged).await;
    let staging = store.cache.join("registry");
    tokio::fs::create_dir_all(&staging).await.map_err(io(staging.display()))?;
    let staged = tempfile::NamedTempFile::new_in(&staging).map_err(io(staging.display()))?;
    let mut file = tokio::fs::File::from_std(staged.as_file().try_clone().map_err(io(staged.path().display()))?);
    let descriptor = OciDescriptor {
      media_type: IMAGE_LAYER_GZIP_MEDIA_TYPE.to_string(),
      digest: layer.blob.to_string(),
      size: i64::try_from(layer.size).unwrap_or(i64::MAX),
      ..Default::default()
    };
    // NOTICE: `pull_blob` verifies the compressed digest as it reads.
    client.pull_blob(&tagged, &descriptor, &mut file).await.map_err(|source| Error::Registry {
      reference: layer.reference.clone(),
      source,
    })?;
    file.sync_all().await.map_err(io(staged.path().display()))?;
    drop(file);

    let store = store.clone();
    let reference = layer.reference.clone();
    let expected = layer.config.diff_id;
    tokio::task::spawn_blocking(move || -> Result<Digest> {
      let compressed = std::fs::File::open(staged.path()).map_err(io(staged.path().display()))?;
      let decompressed = flate2::read::GzDecoder::new(std::io::BufReader::new(compressed));
      store.put_expecting(decompressed, &expected).map_err(|e| match e {
        super::Error::Unexpected { expected, actual } => Error::DiffId {
          reference,
          expected,
          actual,
        },
        e => Error::Store(e),
      })
    })
    .await
    .map_err(|e| Error::Io {
      context: format!("storing pulled layer {}", layer.reference),
      source: std::io::Error::other(e.to_string()),
    })?
  }

  /// Pushes the stored layer `config.diff_id` as the artifact of the cache
  /// key `config.key`: its gzip blob, `config` as its config blob, and the
  /// manifest under the key's tag, replacing whatever the tag named.
  // NOTICE: a later push of the same key replaces the tag (last writer
  // wins). Builds are not reproducible, so two hosts racing on one key may
  // push different layers; either is a valid build of the key, and blobs
  // stay pullable by digest.
  pub async fn push(&self, store: &Store, config: &LayerConfig) -> Result<Pushed> {
    let reference = self.repository.reference(&config.key);
    let tagged = self.repository.tagged(&config.key);
    let registry_error = |source| Error::Registry {
      reference: reference.clone(),
      source,
    };

    let layer = store.open_layer(&config.diff_id)?;
    let diff_id = config.diff_id;
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
      context: format!("compressing layer {}", config.diff_id),
      source: std::io::Error::other(e.to_string()),
    })??;
    let config_bytes = serde_json::to_vec(config).map_err(|e| Error::Io {
      context: format!("encoding the config of layer {}", config.diff_id),
      source: std::io::Error::other(e),
    })?;
    let config_digest = Digest::of(&config_bytes);

    let client = self.client(&tagged).await;
    client.push_blob(&tagged, &config_bytes, &config_digest.to_string()).await.map_err(registry_error)?;
    client.push_blob(&tagged, &blob_bytes, &blob.to_string()).await.map_err(registry_error)?;
    let size = blob_bytes.len() as u64;
    let manifest = OciImageManifest {
      media_type: Some(OCI_IMAGE_MEDIA_TYPE.to_string()),
      config: OciDescriptor {
        media_type: CONFIG_MEDIA_TYPE.to_string(),
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
      ..Default::default()
    };
    // `push_manifest` serializes canonically; the digest of those bytes is
    // the manifest's, which the URL it returns ends with.
    let url = client.push_manifest(&tagged, &OciManifest::Image(manifest.clone())).await.map_err(registry_error)?;
    let manifest_digest = match url.rsplit('/').next().map(Digest::parse) {
      Some(Ok(digest)) => digest,
      _ => {
        // A registry that answers with the tag's URL: ask it.
        let digest = client.fetch_manifest_digest(&tagged, &RegistryAuth::Anonymous).await.map_err(registry_error)?;
        Digest::parse(&digest).map_err(|e| Error::Invalid {
          reference: reference.clone(),
          reason: format!("manifest digest {digest}: {e}"),
        })?
      }
    };
    Ok(Pushed {
      reference,
      manifest: manifest_digest,
      blob,
      size,
    })
  }

  /// A client for one operation, with the credentials the Docker credential
  /// store holds for the registry now.
  // NOTICE: a client per operation, so credentials a helper rotates (such as
  // `docker-credential-ecr-login`'s 12-hour tokens) are read again instead of
  // kept for the daemon's lifetime by `oci-client`'s auth store.
  async fn client(&self, tagged: &oci_client::Reference) -> oci_client::Client {
    let protocol = if self.repository.plain_http {
      ClientProtocol::Http
    } else {
      ClientProtocol::Https
    };
    let client = oci_client::Client::new(ClientConfig {
      protocol,
      user_agent: concat!("needy/", env!("CARGO_PKG_VERSION")),
      ..Default::default()
    });
    // NOTICE: credentials are never sent over plain HTTP. A plain-HTTP
    // registry is an explicit opt-in for in-cluster tests (spec D5).
    let auth = if self.repository.plain_http {
      RegistryAuth::Anonymous
    } else {
      devcontainer_features::fetch::resolve_registry_auth(self.repository.registry.clone()).await
    };
    client.store_auth_if_needed(tagged.resolve_registry(), &auth).await;
    client
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

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
