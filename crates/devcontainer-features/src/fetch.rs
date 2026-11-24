//! OCI Distribution v2 Dev Container feature artifact fetcher and
//! content-addressed cache, built on `oci-client` and `docker_credential`.
//!
//! Provenance:
//! - Source: https://github.com/opencontainers/distribution-spec/blob/v1.1.0/spec.md#pulling-manifests
//! - Reference: https://containers.dev/implementors/features-distribution/ (retrieved 2026-11-21)

use std::collections::HashMap;
use std::fmt;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use docker_credential::{CredentialRetrievalError, DockerCredential};
use jsonc_parser::{ParseOptions, parse_to_serde_value};
use oci_client::client::{ClientConfig, ClientProtocol};
use oci_client::errors::{DigestError, OciDistributionError};
use oci_client::secrets::RegistryAuth;
use serde::Deserialize;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::watch;

use crate::digest::Digest;

/// Accepted OCI/Docker manifest media types when pulling a feature manifest.
// Source: https://github.com/opencontainers/distribution-spec/blob/v1.1.0/spec.md#pulling-manifests
const ACCEPTED_MANIFEST_MEDIA_TYPES: &[&str] = &[
  "application/vnd.oci.image.manifest.v1+json",
  "application/vnd.docker.distribution.manifest.v2+json",
];

/// Media type for a Dev Container feature tarball layer in an OCI manifest.
// Reference: https://containers.dev/implementors/features-distribution/ (retrieved 2026-11-21)
const FEATURE_LAYER_MEDIA_TYPE: &str = "application/vnd.devcontainers.layer.v1+tar";

/// Filename of the feature metadata document inside a Dev Container feature tar.
const METADATA_FILE: &str = "devcontainer-feature.json";

/// Filename of the cached feature archive inside a cache entry directory.
const ARCHIVE_FILE: &str = "feature.tar";

/// Controls when [`FeatureStore`] consults a remote OCI registry for
/// tag-based feature references, matching Kubernetes `imagePullPolicy`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PullPolicy {
  /// Use a locally cached tag resolution when present; pull from the registry
  /// only when not yet cached (the default).
  #[default]
  IfNotPresent,
  /// Always query the remote OCI registry to resolve tags (still reusing the
  /// locally cached archive blob when the resolved digest matches).
  Always,
  /// Never contact a remote OCI registry; fail with [`FetchError::NotCached`]
  /// if the feature is not already present in the local cache.
  Never,
}

/// Errors fetching or reading a cached OCI Dev Container feature.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
  /// The feature reference string is not a valid OCI reference.
  #[error("invalid OCI feature reference {0:?}: {1}")]
  InvalidReference(String, String),
  /// The feature is not present in the local cache and the pull policy forbids
  /// network access.
  #[error("feature {0:?} is not in the local cache (pull policy: never)")]
  NotCached(String),
  /// OCI registry or transport error reported by `oci-client`.
  #[error("OCI registry error: {0}")]
  Registry(#[from] OciDistributionError),
  /// The OCI manifest JSON was malformed or lacked a usable feature layer.
  #[error("invalid OCI manifest: {0}")]
  InvalidManifest(String),
  /// A manifest or blob did not match its expected SHA-256 digest.
  #[error("{what} digest mismatch: expected {expected}, got {actual}")]
  DigestMismatch {
    what: &'static str,
    expected: Digest,
    actual: Digest,
  },
  /// The feature tar archive could not be read or lacked valid metadata.
  #[error("invalid feature archive: {0}")]
  InvalidArchive(String),
  /// A concurrent in-flight fetch for the same reference failed.
  #[error("in-flight feature fetch failed: {0}")]
  InFlight(String),
  /// Local cache filesystem error.
  #[error("{context}: {source}")]
  Io {
    context: String,
    #[source]
    source: std::io::Error,
  },
}

pub type Result<T> = std::result::Result<T, FetchError>;

fn io(context: impl fmt::Display) -> impl FnOnce(std::io::Error) -> FetchError {
  move |source| FetchError::Io {
    context: context.to_string(),
    source,
  }
}

/// The target selector of an OCI feature reference: either a tag or a pinned
/// SHA-256 digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefTarget {
  Tag(String),
  Digest(Digest),
}

/// A parsed OCI Dev Container feature reference, e.g.
/// `ghcr.io/devcontainers/features/node:1` or
/// `ghcr.io/devcontainers/features/node@sha256:...`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
  pub registry: String,
  pub repository: String,
  pub target: RefTarget,
}

impl Reference {
  /// Parses an OCI feature reference. Defaults to tag `latest` when neither a
  /// tag nor a `@sha256:...` digest is specified.
  pub fn parse(raw: &str) -> Result<Self> {
    let bad = |reason: &str| FetchError::InvalidReference(raw.to_string(), reason.to_string());
    if raw.is_empty() || raw.chars().any(char::is_whitespace) {
      return Err(bad("must be non-empty and contain no whitespace"));
    }
    if raw.contains("://") {
      return Err(bad("must not include a URL scheme"));
    }

    let (without_digest, parsed_digest) = match raw.split_once('@') {
      Some((prefix, digest_part)) => {
        if digest_part.contains('@') {
          return Err(bad("multiple `@` digest separators"));
        }
        let digest = Digest::parse(digest_part).map_err(|e| FetchError::InvalidReference(raw.to_string(), e.to_string()))?;
        (prefix, Some(digest))
      }
      None => (raw, None),
    };

    let Some((registry, rest)) = without_digest.split_once('/') else {
      return Err(bad("expected `<registry>/<repository>[:<tag>|@<digest>]`"));
    };
    if registry.is_empty() || !(registry.contains('.') || registry.contains(':') || registry == "localhost") {
      return Err(bad("registry host must contain `.`, `:`, or be `localhost`"));
    }
    if rest.is_empty() {
      return Err(bad("repository path must not be empty"));
    }

    let (repository, parsed_tag) = match rest.split_once(':') {
      Some((repo, tag)) => {
        if tag.is_empty() || tag.contains('/') || tag.contains(':') {
          return Err(bad("tag must be non-empty and contain no `/` or `:`"));
        }
        (repo, Some(tag))
      }
      None => (rest, None),
    };

    if repository.is_empty()
      || repository.starts_with('/')
      || repository.ends_with('/')
      || repository.split('/').any(|seg| seg.is_empty() || seg == "." || seg == "..")
    {
      return Err(bad("repository path contains empty or traversal segments"));
    }

    let target = match (parsed_digest, parsed_tag) {
      (Some(digest), _) => RefTarget::Digest(digest),
      (None, Some(tag)) => RefTarget::Tag(tag.to_string()),
      (None, None) => RefTarget::Tag("latest".to_string()),
    };

    Ok(Self {
      registry: registry.to_string(),
      repository: repository.to_string(),
      target,
    })
  }

  /// The canonical identity of this feature without any tag or digest:
  /// `<registry>/<repository>`.
  pub fn canonical(&self) -> String {
    format!("{}/{}", self.registry, self.repository)
  }

  /// The tag if this reference selects by tag.
  pub fn tag(&self) -> Option<&str> {
    match &self.target {
      RefTarget::Tag(tag) => Some(tag.as_str()),
      RefTarget::Digest(_) => None,
    }
  }

  /// The pinned digest if this reference selects by digest.
  pub fn digest(&self) -> Option<Digest> {
    match self.target {
      RefTarget::Tag(_) => None,
      RefTarget::Digest(digest) => Some(digest),
    }
  }

  /// Returns a copy of this reference pinned to `digest`.
  pub fn with_digest(&self, digest: Digest) -> Self {
    Self {
      registry: self.registry.clone(),
      repository: self.repository.clone(),
      target: RefTarget::Digest(digest),
    }
  }

  fn to_oci_reference(&self, registry_override: Option<&str>) -> oci_client::Reference {
    let registry = registry_override.unwrap_or(&self.registry).to_string();
    match &self.target {
      RefTarget::Tag(tag) => oci_client::Reference::with_tag(registry, self.repository.clone(), tag.clone()),
      RefTarget::Digest(digest) => oci_client::Reference::with_digest(registry, self.repository.clone(), digest.to_string()),
    }
  }

  fn target_segment(&self) -> String {
    match &self.target {
      RefTarget::Tag(tag) => tag.clone(),
      RefTarget::Digest(digest) => digest.to_string(),
    }
  }
}

impl FromStr for Reference {
  type Err = FetchError;

  fn from_str(s: &str) -> Result<Self> {
    Self::parse(s)
  }
}

impl fmt::Display for Reference {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match &self.target {
      RefTarget::Tag(tag) => write!(f, "{}/{}:{tag}", self.registry, self.repository),
      RefTarget::Digest(digest) => write!(f, "{}/{}@{digest}", self.registry, self.repository),
    }
  }
}

/// Strips any tag or `@sha256:...` digest from an OCI feature reference,
/// returning its canonical `registry/repository` identity.
pub fn canonical_ref(raw: &str) -> Result<String> {
  Ok(Reference::parse(raw)?.canonical())
}

/// A fetched and verified Dev Container feature stored in the local cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedFeature {
  /// SHA-256 digest of the OCI manifest pinning this feature artifact.
  pub digest: Digest,
  /// Contents of `devcontainer-feature.json` extracted from the feature tar.
  pub metadata_json: String,
  /// Path to the cached `feature.tar` archive.
  pub archive_path: PathBuf,
}

impl FetchedFeature {
  /// Reads the cached feature archive as an uncompressed tar byte vector,
  /// decompressing gzip when the registry served a `.tar.gz` blob.
  pub fn uncompressed_tar(&self) -> Result<Vec<u8>> {
    let raw = std::fs::read(&self.archive_path).map_err(io(self.archive_path.display()))?;
    // Gzip magic bytes (`0x1f 0x8b`, RFC 1952 section 2.3.1).
    if raw.len() >= 2 && raw[0] == 0x1f && raw[1] == 0x8b {
      let mut out = Vec::new();
      flate2::read::GzDecoder::new(raw.as_slice())
        .read_to_end(&mut out)
        .map_err(|e| FetchError::InvalidArchive(format!("cannot gunzip {}: {e}", self.archive_path.display())))?;
      Ok(out)
    } else {
      Ok(raw)
    }
  }
}

type FeatureFlightResult = std::result::Result<FetchedFeature, String>;
type FeatureFlightMap = HashMap<String, watch::Receiver<Option<FeatureFlightResult>>>;

/// Fetches Dev Container feature artifacts over OCI Distribution v2 and caches
/// them atomically under `cache_dir/features/sha256-<manifest-hex>/`.
#[derive(Clone)]
pub struct FeatureStore {
  cache_dir: PathBuf,
  client: oci_client::Client,
  protocol: ClientProtocol,
  registry_override: Option<String>,
  no_proxy: Option<String>,
  in_flight: Arc<Mutex<FeatureFlightMap>>,
}

impl fmt::Debug for FeatureStore {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("FeatureStore")
      .field("cache_dir", &self.cache_dir)
      .field("protocol", &self.protocol)
      .field("registry_override", &self.registry_override)
      .finish_non_exhaustive()
  }
}

impl FeatureStore {
  /// Creates a new feature store caching under `cache_dir` and talking HTTPS
  /// to OCI registries.
  pub fn new(cache_dir: impl Into<PathBuf>) -> Result<Self> {
    let protocol = ClientProtocol::Https;
    let client = build_oci_client(protocol.clone(), None);
    Ok(Self {
      cache_dir: cache_dir.into(),
      client,
      protocol,
      registry_override: None,
      no_proxy: None,
      in_flight: Arc::default(),
    })
  }

  /// Overrides the URL scheme (`"https"` by default; `"http"` for plain HTTP
  /// test registries).
  pub fn with_scheme(mut self, scheme: impl Into<String>) -> Self {
    let scheme = scheme.into();
    self.protocol = if scheme.eq_ignore_ascii_case("http") {
      ClientProtocol::Http
    } else {
      ClientProtocol::Https
    };
    self.client = build_oci_client(self.protocol.clone(), self.no_proxy.clone());
    self
  }

  /// Overrides the registry base URL (for example `"http://127.0.0.1:12345"`),
  /// routing all fetches to that endpoint regardless of `reference.registry`.
  pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
    let raw = base_url.into();
    let trimmed = raw.trim_end_matches('/');
    let (protocol, authority) = if let Some(rest) = trimmed.strip_prefix("http://") {
      (ClientProtocol::Http, rest)
    } else if let Some(rest) = trimmed.strip_prefix("https://") {
      (ClientProtocol::Https, rest)
    } else {
      (self.protocol.clone(), trimmed)
    };
    self.protocol = protocol;
    self.registry_override = Some(authority.to_string());
    self.client = build_oci_client(self.protocol.clone(), self.no_proxy.clone());
    self
  }

  /// Disables HTTP proxies for loopback/local test registries.
  pub fn without_proxy(mut self) -> Result<Self> {
    self.no_proxy = Some("*".to_string());
    self.client = build_oci_client(self.protocol.clone(), self.no_proxy.clone());
    Ok(self)
  }

  /// Looks up a feature already cached under `digest`, making no network calls.
  pub fn get_cached(&self, digest: &Digest) -> Result<Option<FetchedFeature>> {
    let dir = self.entry_dir(digest);
    let archive_path = dir.join(ARCHIVE_FILE);
    let metadata_path = dir.join(METADATA_FILE);
    if !archive_path.is_file() || !metadata_path.is_file() {
      return Ok(None);
    }
    let metadata_json = std::fs::read_to_string(&metadata_path).map_err(io(metadata_path.display()))?;
    Ok(Some(FetchedFeature {
      digest: *digest,
      metadata_json,
      archive_path,
    }))
  }

  /// Looks up a feature already cached under `reference` (by digest for
  /// `@sha256:...` references, or via the local tag index for `:tag`
  /// references), making no network calls.
  pub fn get_cached_by_ref(&self, reference: &Reference) -> Result<Option<FetchedFeature>> {
    if let Some(pinned) = reference.digest() {
      return self.get_cached(&pinned);
    }
    let Some(path) = self.tag_index_path(reference) else {
      return Ok(None);
    };
    let text = match std::fs::read_to_string(&path) {
      Ok(text) => text,
      Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
      Err(err) => return Err(io(path.display())(err)),
    };
    let Ok(digest) = Digest::parse(text.trim()) else {
      return Ok(None);
    };
    self.get_cached(&digest)
  }

  /// Fetches `reference` using [`PullPolicy::IfNotPresent`] (preferring a
  /// locally cached tag or digest entry before contacting the registry).
  /// Concurrent fetches for the same reference share one in-flight download.
  pub async fn fetch(&self, reference: &Reference) -> Result<FetchedFeature> {
    self.fetch_with_policy(reference, PullPolicy::IfNotPresent).await
  }

  /// Fetches `reference` according to `policy`. Concurrent fetches for the
  /// same reference share one in-flight download.
  pub async fn fetch_with_policy(&self, reference: &Reference, policy: PullPolicy) -> Result<FetchedFeature> {
    match policy {
      PullPolicy::Never => {
        return self.get_cached_by_ref(reference)?.ok_or_else(|| FetchError::NotCached(reference.to_string()));
      }
      PullPolicy::IfNotPresent => {
        if let Some(cached) = self.get_cached_by_ref(reference)? {
          return Ok(cached);
        }
      }
      PullPolicy::Always => {
        if let Some(pinned) = reference.digest()
          && let Some(cached) = self.get_cached(&pinned)?
        {
          return Ok(cached);
        }
      }
    }

    let registry = self.registry_override.as_deref().unwrap_or(&reference.registry);
    let flight_key = format!("{registry}/{}:{}", reference.repository, reference.target_segment());

    enum Role {
      Leader(watch::Sender<Option<FeatureFlightResult>>),
      Waiter(watch::Receiver<Option<FeatureFlightResult>>),
      Cached(FetchedFeature),
    }

    let role = {
      let mut in_flight = self.in_flight.lock().unwrap_or_else(|p| p.into_inner());
      let cached = match policy {
        PullPolicy::IfNotPresent => self.get_cached_by_ref(reference)?,
        PullPolicy::Always => match reference.digest() {
          Some(pinned) => self.get_cached(&pinned)?,
          None => None,
        },
        PullPolicy::Never => None,
      };
      if let Some(cached) = cached {
        Role::Cached(cached)
      } else if let Some(rx) = in_flight.get(&flight_key) {
        Role::Waiter(rx.clone())
      } else {
        let (tx, rx) = watch::channel(None);
        in_flight.insert(flight_key.clone(), rx);
        Role::Leader(tx)
      }
    };

    match role {
      Role::Cached(cached) => Ok(cached),
      Role::Waiter(mut rx) => {
        let result = rx
          .wait_for(Option::is_some)
          .await
          .map_err(|_| FetchError::InFlight(format!("in-flight feature fetch for {reference} ended without a result")))?
          .clone()
          .expect("wait_for guarantees Some");
        result.map_err(FetchError::InFlight)
      }
      Role::Leader(tx) => {
        struct Guard {
          in_flight: Arc<Mutex<FeatureFlightMap>>,
          key: String,
        }
        impl Drop for Guard {
          fn drop(&mut self) {
            self.in_flight.lock().unwrap_or_else(|p| p.into_inner()).remove(&self.key);
          }
        }
        let _guard = Guard {
          in_flight: self.in_flight.clone(),
          key: flight_key,
        };

        let fetched = self.fetch_uncached(reference).await;
        match &fetched {
          Ok(feature) => {
            let _ = tx.send(Some(Ok(feature.clone())));
          }
          Err(err) => {
            let _ = tx.send(Some(Err(err.to_string())));
          }
        }
        fetched
      }
    }
  }

  /// Fetches `reference` pinned to `pinned` (from a lockfile), hitting the
  /// local cache with zero HTTP requests when already present and otherwise
  /// pulling directly by `@sha256:...` without resolving any tag.
  pub async fn fetch_pinned(&self, reference: &Reference, pinned: &Digest) -> Result<FetchedFeature> {
    self.fetch_pinned_with_policy(reference, pinned, PullPolicy::IfNotPresent).await
  }

  /// Fetches `reference` pinned to `pinned` according to `policy`.
  pub async fn fetch_pinned_with_policy(&self, reference: &Reference, pinned: &Digest, policy: PullPolicy) -> Result<FetchedFeature> {
    if let Some(cached) = self.get_cached(pinned)? {
      return Ok(cached);
    }
    if policy == PullPolicy::Never {
      return Err(FetchError::NotCached(format!("{reference} ({pinned})")));
    }
    self.fetch_with_policy(&reference.with_digest(*pinned), policy).await
  }

  /// Writes `blob_bytes` and `metadata_json` atomically into the cache under
  /// `digest`. Useful for consumers that materialize non-OCI (e.g. built-in or
  /// local) features into the same content-addressed cache.
  pub fn cache_atomically(&self, digest: &Digest, blob_bytes: &[u8], metadata_json: &str) -> Result<FetchedFeature> {
    let features_dir = self.cache_dir.join("features");
    std::fs::create_dir_all(&features_dir).map_err(io(features_dir.display()))?;
    let target_dir = self.entry_dir(digest);

    let staged = tempfile::Builder::new().prefix(".feature-").tempdir_in(&features_dir).map_err(io(features_dir.display()))?;

    let staged_archive = staged.path().join(ARCHIVE_FILE);
    write_and_sync(&staged_archive, blob_bytes)?;

    let staged_metadata = staged.path().join(METADATA_FILE);
    write_and_sync(&staged_metadata, metadata_json.as_bytes())?;

    let staged_path = staged.keep();
    if let Err(err) = std::fs::rename(&staged_path, &target_dir) {
      // Best-effort cleanup of the unused staging directory; the primary error
      // (or concurrent cache hit below) takes precedence.
      let _ = std::fs::remove_dir_all(&staged_path);
      if !(target_dir.join(ARCHIVE_FILE).is_file() && target_dir.join(METADATA_FILE).is_file()) {
        return Err(io(target_dir.display())(err));
      }
    }

    Ok(FetchedFeature {
      digest: *digest,
      metadata_json: metadata_json.to_string(),
      archive_path: target_dir.join(ARCHIVE_FILE),
    })
  }

  async fn fetch_uncached(&self, reference: &Reference) -> Result<FetchedFeature> {
    let oci_ref = reference.to_oci_reference(self.registry_override.as_deref());
    let auth = if self.registry_override.is_some() {
      RegistryAuth::Anonymous
    } else {
      resolve_registry_auth(reference.registry.clone()).await
    };

    let (manifest_bytes, _) = self
      .client
      .pull_manifest_raw(&oci_ref, &auth, ACCEPTED_MANIFEST_MEDIA_TYPES)
      .await
      .map_err(|err| map_oci_error(err, "feature manifest"))?;

    let manifest_digest = Digest::of(&manifest_bytes);
    if let Some(expected) = reference.digest()
      && expected != manifest_digest
    {
      return Err(FetchError::DigestMismatch {
        what: "feature manifest",
        expected,
        actual: manifest_digest,
      });
    }

    if let Some(cached) = self.get_cached(&manifest_digest)? {
      self.record_tag(reference, &manifest_digest)?;
      return Ok(cached);
    }

    let manifest: OciManifest =
      serde_json::from_slice(&manifest_bytes).map_err(|e| FetchError::InvalidManifest(format!("cannot parse JSON: {e}")))?;
    let layer = select_feature_layer(&manifest)?;
    let layer_digest =
      Digest::parse(&layer.digest).map_err(|e| FetchError::InvalidManifest(format!("invalid layer digest {}: {e}", layer.digest)))?;

    let features_dir = self.cache_dir.join("features");
    tokio::fs::create_dir_all(&features_dir).await.map_err(io(features_dir.display()))?;
    let features_dir_for_stage = features_dir.clone();
    let staged = tokio::task::spawn_blocking(move || {
      tempfile::Builder::new().prefix(".feature-").tempdir_in(&features_dir_for_stage).map_err(io(features_dir_for_stage.display()))
    })
    .await
    .map_err(|e| FetchError::Io {
      context: format!("creating feature staging directory panicked: {e}"),
      source: std::io::Error::other(e.to_string()),
    })??;

    let staged_archive = staged.path().join(ARCHIVE_FILE);
    let mut archive_file = tokio::fs::File::create(&staged_archive).await.map_err(io(staged_archive.display()))?;
    if let Err(err) = self.client.pull_blob(&oci_ref, layer_digest.to_string().as_str(), &mut archive_file).await {
      drop(archive_file);
      drop(staged);
      return Err(map_oci_error(err, "feature blob"));
    }
    archive_file.flush().await.map_err(io(staged_archive.display()))?;
    archive_file.sync_all().await.map_err(io(staged_archive.display()))?;
    drop(archive_file);

    let target_dir = self.entry_dir(&manifest_digest);
    let fetched = tokio::task::spawn_blocking(move || {
      let metadata_json = extract_metadata_from_file(&staged_archive)?;
      let staged_metadata = staged.path().join(METADATA_FILE);
      write_and_sync(&staged_metadata, metadata_json.as_bytes())?;

      let staged_path = staged.keep();
      if let Err(err) = std::fs::rename(&staged_path, &target_dir) {
        // Best-effort cleanup of the unused staging directory; the primary
        // error (or concurrent cache hit below) takes precedence.
        let _ = std::fs::remove_dir_all(&staged_path);
        if !(target_dir.join(ARCHIVE_FILE).is_file() && target_dir.join(METADATA_FILE).is_file()) {
          return Err(io(target_dir.display())(err));
        }
      }

      Ok(FetchedFeature {
        digest: manifest_digest,
        metadata_json,
        archive_path: target_dir.join(ARCHIVE_FILE),
      })
    })
    .await
    .map_err(|e| FetchError::Io {
      context: format!("caching feature archive panicked: {e}"),
      source: std::io::Error::other(e.to_string()),
    })??;
    self.record_tag(reference, &manifest_digest)?;
    Ok(fetched)
  }

  fn entry_dir(&self, digest: &Digest) -> PathBuf {
    self.cache_dir.join("features").join(format!("sha256-{}", digest.hex()))
  }

  fn tag_index_path(&self, reference: &Reference) -> Option<PathBuf> {
    let RefTarget::Tag(tag) = &reference.target else {
      return None;
    };
    let registry = self.registry_override.as_deref().unwrap_or(&reference.registry);
    let key = format!("{registry}/{}:{tag}", reference.repository);
    let hash = Digest::of(key.as_bytes());
    Some(self.cache_dir.join("features").join("tags").join(format!("sha256-{}", hash.hex())))
  }

  fn record_tag(&self, reference: &Reference, digest: &Digest) -> Result<()> {
    let Some(path) = self.tag_index_path(reference) else {
      return Ok(());
    };
    let Some(dir) = path.parent() else {
      return Ok(());
    };
    std::fs::create_dir_all(dir).map_err(io(dir.display()))?;
    let mut staged = tempfile::Builder::new().prefix(".tag-").tempfile_in(dir).map_err(io(dir.display()))?;
    staged.write_all(format!("{digest}\n").as_bytes()).map_err(io(dir.display()))?;
    staged.as_file().sync_all().map_err(io(dir.display()))?;
    staged.persist(&path).map_err(|e| io(path.display())(e.error))?;
    Ok(())
  }
}

fn build_oci_client(protocol: ClientProtocol, no_proxy: Option<String>) -> oci_client::Client {
  let config = ClientConfig {
    protocol,
    user_agent: concat!("devcontainer-features/", env!("CARGO_PKG_VERSION")),
    no_proxy,
    ..Default::default()
  };
  oci_client::Client::new(config)
}

/// The credentials the Docker credential store (`~/.docker/config.json`, its
/// `credsStore` and `credHelpers`) holds for `registry` (a host, with its
/// port if any), or anonymous access when it holds none or the lookup fails.
pub async fn resolve_registry_auth(registry: String) -> RegistryAuth {
  // Credential helpers can spawn a subprocess, so run lookup off the async runtime.
  tokio::task::spawn_blocking(move || match docker_credential::get_credential(&registry) {
    Ok(DockerCredential::UsernamePassword(username, password)) => RegistryAuth::Basic(username, password),
    Ok(DockerCredential::IdentityToken(token)) => RegistryAuth::Bearer(token),
    Err(CredentialRetrievalError::ConfigNotFound | CredentialRetrievalError::NoCredentialConfigured) => RegistryAuth::Anonymous,
    Err(err) => {
      tracing::debug!("docker credential lookup for {registry} fell back to anonymous: {err}");
      RegistryAuth::Anonymous
    }
  })
  .await
  .unwrap_or(RegistryAuth::Anonymous)
}

fn map_oci_error(err: OciDistributionError, what: &'static str) -> FetchError {
  if let OciDistributionError::DigestError(DigestError::VerificationError { expected, actual }) = &err
    && let (Ok(expected), Ok(actual)) = (Digest::parse(expected), Digest::parse(actual))
  {
    return FetchError::DigestMismatch {
      what,
      expected,
      actual,
    };
  }
  FetchError::Registry(err)
}

fn write_and_sync(path: &Path, bytes: &[u8]) -> Result<()> {
  let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(io(path.display()))?;
  file.write_all(bytes).map_err(io(path.display()))?;
  file.sync_all().map_err(io(path.display()))?;
  Ok(())
}

#[derive(Deserialize)]
struct OciManifest {
  #[serde(default)]
  layers: Vec<OciDescriptor>,
}

#[derive(Deserialize)]
struct OciDescriptor {
  #[serde(rename = "mediaType", default)]
  media_type: String,
  digest: String,
}

fn select_feature_layer(manifest: &OciManifest) -> Result<&OciDescriptor> {
  if let Some(layer) = manifest.layers.iter().find(|l| l.media_type == FEATURE_LAYER_MEDIA_TYPE) {
    return Ok(layer);
  }
  if manifest.layers.len() == 1 {
    return Ok(&manifest.layers[0]);
  }
  Err(FetchError::InvalidManifest(format!(
    "expected layer with mediaType {FEATURE_LAYER_MEDIA_TYPE:?} or a single layer, found {} layers",
    manifest.layers.len()
  )))
}

/// Extracts and validates `devcontainer-feature.json` from a Dev Container
/// feature tarball file (uncompressed or gzip-compressed).
fn extract_metadata_from_file(path: &Path) -> Result<String> {
  let mut file = std::fs::File::open(path).map_err(io(path.display()))?;
  let mut magic = [0u8; 2];
  let read = file.read(&mut magic).map_err(io(path.display()))?;
  file.seek(SeekFrom::Start(0)).map_err(io(path.display()))?;
  let reader = std::io::BufReader::new(file);
  // Gzip magic bytes (`0x1f 0x8b`, RFC 1952 section 2.3.1).
  if read == 2 && magic == [0x1f, 0x8b] {
    extract_metadata_from_reader(flate2::read::GzDecoder::new(reader))
  } else {
    extract_metadata_from_reader(reader)
  }
}

fn extract_metadata_from_reader(reader: impl std::io::Read) -> Result<String> {
  let mut archive = tar::Archive::new(reader);
  let entries = archive.entries().map_err(|e| FetchError::InvalidArchive(format!("cannot read tar entries: {e}")))?;
  for entry in entries {
    let mut entry = entry.map_err(|e| FetchError::InvalidArchive(format!("corrupt tar entry: {e}")))?;
    let path = entry.path().map_err(|e| FetchError::InvalidArchive(format!("invalid tar entry path: {e}")))?;
    if is_metadata_entry(&path) {
      let mut text = String::new();
      entry.read_to_string(&mut text).map_err(|e| FetchError::InvalidArchive(format!("cannot read {METADATA_FILE}: {e}")))?;
      parse_to_serde_value::<Option<serde_json::Value>>(&text, &ParseOptions::default())
        .map_err(|e| FetchError::InvalidArchive(format!("{METADATA_FILE} is not valid JSONC: {e}")))?
        .ok_or_else(|| FetchError::InvalidArchive(format!("{METADATA_FILE} is empty")))?;
      return Ok(text);
    }
  }
  Err(FetchError::InvalidArchive(format!("feature archive is missing {METADATA_FILE}")))
}

fn is_metadata_entry(path: &Path) -> bool {
  let mut normal = path.components().filter(|c| !matches!(c, Component::CurDir));
  matches!(
    (normal.next(), normal.next()),
    (Some(Component::Normal(name)), None) if name == METADATA_FILE
  )
}

#[cfg(test)]
mod tests {
  use std::sync::atomic::{AtomicUsize, Ordering};

  use tokio::io::AsyncReadExt as _;

  use super::*;

  fn make_feature_tar(metadata_json: &str, install_sh: &str) -> Vec<u8> {
    let mut tar = tar::Builder::new(Vec::new());
    for (name, content) in [
      ("devcontainer-feature.json", metadata_json.as_bytes()),
      ("install.sh", install_sh.as_bytes()),
    ] {
      let mut header = tar::Header::new_gnu();
      header.set_size(content.len() as u64);
      header.set_mode(0o644);
      header.set_cksum();
      tar.append_data(&mut header, name, content).unwrap();
    }
    tar.into_inner().unwrap()
  }

  fn make_oci_manifest(layer_digest: &Digest, layer_size: usize) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
      "schemaVersion": 2,
      "mediaType": "application/vnd.oci.image.manifest.v1+json",
      "config": {
        "mediaType": "application/vnd.devcontainers",
        "digest": Digest::of(b"{}").to_string(),
        "size": 2
      },
      "layers": [
        {
          "mediaType": "application/vnd.devcontainers.layer.v1+tar",
          "digest": layer_digest.to_string(),
          "size": layer_size
        }
      ]
    }))
    .unwrap()
  }

  #[derive(Clone)]
  struct MockRegistryState {
    repo: String,
    tag: String,
    manifest_bytes: Vec<u8>,
    manifest_digest: Digest,
    blob_digest: Digest,
    blob_bytes: Vec<u8>,
    required_token: String,
    requests: Arc<AtomicUsize>,
  }

  async fn spawn_mock_registry(state: MockRegistryState) -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{addr}");
    let requests = state.requests.clone();
    let server_url = base_url.clone();

    tokio::spawn(async move {
      while let Ok((mut stream, _)) = listener.accept().await {
        let state = state.clone();
        let server_url = server_url.clone();
        tokio::spawn(async move {
          let mut buf = Vec::new();
          let mut chunk = [0u8; 1024];
          while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            match stream.read(&mut chunk).await {
              Ok(0) | Err(_) => return,
              Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
          }
          state.requests.fetch_add(1, Ordering::SeqCst);

          let raw = String::from_utf8_lossy(&buf);
          let mut lines = raw.lines();
          let req_line = lines.next().unwrap_or("");
          let target = req_line.split_whitespace().nth(1).unwrap_or("/");
          let auth = lines
            .find(|line| line.to_ascii_lowercase().starts_with("authorization:"))
            .and_then(|line| line.split_once(':'))
            .map(|(_, v)| v.trim().to_string());

          let expected_auth = format!("Bearer {}", state.required_token);
          let manifest_by_tag = format!("/v2/{}/manifests/{}", state.repo, state.tag);
          let manifest_by_digest = format!("/v2/{}/manifests/{}", state.repo, state.manifest_digest);
          let blob_path = format!("/v2/{}/blobs/{}", state.repo, state.blob_digest);

          let (status, extra_headers, body) = if target.starts_with("/token") {
            let body = serde_json::to_vec(&serde_json::json!({ "token": state.required_token })).unwrap();
            ("200 OK", "Content-Type: application/json\r\n".to_string(), body)
          } else if auth.as_deref() != Some(expected_auth.as_str()) {
            let challenge = format!(
              "WWW-Authenticate: Bearer realm=\"{server_url}/token\",service=\"registry.example.com\",scope=\"repository:{}:pull\"\r\n",
              state.repo
            );
            ("401 Unauthorized", challenge, b"unauthorized".to_vec())
          } else if target == manifest_by_tag || target == manifest_by_digest {
            let headers =
              format!("Content-Type: application/vnd.oci.image.manifest.v1+json\r\nDocker-Content-Digest: {}\r\n", state.manifest_digest);
            ("200 OK", headers, state.manifest_bytes.clone())
          } else if target == blob_path {
            ("200 OK", "Content-Type: application/vnd.devcontainers.layer.v1+tar\r\n".to_string(), state.blob_bytes.clone())
          } else {
            ("404 Not Found", String::new(), b"not found".to_vec())
          };

          let head = format!("HTTP/1.1 {status}\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len());
          if stream.write_all(head.as_bytes()).await.is_ok() {
            let _ = stream.write_all(&body).await;
          }
        });
      }
    });

    (base_url, requests)
  }

  #[test]
  fn reference_parsing_handles_tags_digests_defaults_and_ports() {
    let with_tag: Reference = "ghcr.io/devcontainers/features/node:1".parse().unwrap();
    assert_eq!(with_tag.registry, "ghcr.io");
    assert_eq!(with_tag.repository, "devcontainers/features/node");
    assert_eq!(with_tag.tag(), Some("1"));
    assert_eq!(with_tag.canonical(), "ghcr.io/devcontainers/features/node");

    let default_tag = Reference::parse("ghcr.io/devcontainers/features/node").unwrap();
    assert_eq!(default_tag.tag(), Some("latest"));

    let digest = Digest::of(b"manifest");
    let pinned = Reference::parse(&format!("ghcr.io/devcontainers/features/node@{digest}")).unwrap();
    assert_eq!(pinned.digest(), Some(digest));
    assert_eq!(canonical_ref("ghcr.io/devcontainers/features/node:2").unwrap(), "ghcr.io/devcontainers/features/node");

    assert!(Reference::parse("").is_err());
    assert!(Reference::parse("ghcr.io").is_err());
    assert!(Reference::parse("https://ghcr.io/devcontainers/features/node:1").is_err());
    assert!(Reference::parse("ghcr.io/../escape:1").is_err());
  }

  #[tokio::test]
  async fn fetch_handles_bearer_auth_jsonc_metadata_caching_and_single_flight() {
    let metadata = "{\n  // JSONC comment\n  \"id\": \"node\",\n  \"version\": \"1.5.0\",\n}\n";
    let blob_bytes = make_feature_tar(metadata, "#!/bin/sh\necho installing node\n");
    let blob_digest = Digest::of(&blob_bytes);
    let manifest_bytes = make_oci_manifest(&blob_digest, blob_bytes.len());
    let manifest_digest = Digest::of(&manifest_bytes);

    let (base_url, requests) = spawn_mock_registry(MockRegistryState {
      repo: "devcontainers/features/node".into(),
      tag: "1".into(),
      manifest_bytes,
      manifest_digest,
      blob_digest,
      blob_bytes: blob_bytes.clone(),
      required_token: "test-bearer-token-123".into(),
      requests: Arc::new(AtomicUsize::new(0)),
    })
    .await;

    let cache = tempfile::tempdir().unwrap();
    let store = FeatureStore::new(cache.path()).unwrap().without_proxy().unwrap().with_base_url(&base_url);
    let reference = Reference::parse("ghcr.io/devcontainers/features/node:1").unwrap();

    // Concurrent fetches share one in-flight download.
    let mut handles = Vec::new();
    for _ in 0..4 {
      let store = store.clone();
      let reference = reference.clone();
      handles.push(tokio::spawn(async move { store.fetch(&reference).await }));
    }
    let mut results = Vec::new();
    for handle in handles {
      results.push(handle.await.unwrap().unwrap());
    }
    for fetched in &results {
      assert_eq!(fetched.digest, manifest_digest);
      assert_eq!(fetched.metadata_json, metadata);
      assert_eq!(fetched.uncompressed_tar().unwrap(), blob_bytes);
    }
    let initial_requests = requests.load(Ordering::SeqCst);
    assert!(initial_requests <= 4, "concurrent fetches must share one in-flight download, saw {initial_requests} requests");

    // Pinned cache hit makes zero HTTP requests.
    requests.store(0, Ordering::SeqCst);
    let cached = store.fetch_pinned(&reference, &manifest_digest).await.unwrap();
    assert_eq!(cached.digest, manifest_digest);
    assert_eq!(requests.load(Ordering::SeqCst), 0);
  }

  #[tokio::test]
  async fn corrupt_blob_sha256_fails_with_digest_mismatch_and_leaves_no_partial_cache() {
    let metadata = r#"{"id":"node","version":"1.5.0"}"#;
    let good_blob = make_feature_tar(metadata, "#!/bin/sh\nexit 0\n");
    let expected_blob_digest = Digest::of(&good_blob);
    let manifest_bytes = make_oci_manifest(&expected_blob_digest, good_blob.len());
    let manifest_digest = Digest::of(&manifest_bytes);

    let mut corrupt_blob = good_blob;
    corrupt_blob[0] ^= 0xff;

    let (base_url, _requests) = spawn_mock_registry(MockRegistryState {
      repo: "devcontainers/features/node".into(),
      tag: "1".into(),
      manifest_bytes,
      manifest_digest,
      blob_digest: expected_blob_digest,
      blob_bytes: corrupt_blob,
      required_token: "tok".into(),
      requests: Arc::new(AtomicUsize::new(0)),
    })
    .await;

    let cache = tempfile::tempdir().unwrap();
    let store = FeatureStore::new(cache.path()).unwrap().without_proxy().unwrap().with_base_url(&base_url);
    let reference = Reference::parse("ghcr.io/devcontainers/features/node:1").unwrap();

    let err = store.fetch(&reference).await.unwrap_err();
    assert!(
      matches!(
        err,
        FetchError::DigestMismatch {
          what: "feature blob",
          ..
        }
      ),
      "unexpected error: {err:?}"
    );
    assert!(store.get_cached(&manifest_digest).unwrap().is_none());
  }
}
