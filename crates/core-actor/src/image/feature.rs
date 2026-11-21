//! OCI Dev Container feature artifact fetcher and content-addressed cache.
//!
//! Fetches published Dev Container features (for example
//! `ghcr.io/devcontainers/features/node:1`) over the OCI Distribution v2 HTTP
//! protocol, handles anonymous Bearer token challenges, verifies manifest and
//! blob SHA-256 digests, and caches both the raw feature tar (`feature.tar`)
//! and extracted `devcontainer-feature.json` atomically under
//! `cache_dir/features/sha256-<manifest-hex>/`.
//!
//! Provenance:
//! - Source: https://github.com/opencontainers/distribution-spec/blob/v1.1.0/spec.md#pulling-manifests
//! - Reference: https://containers.dev/implementors/features-distribution/ (retrieved 2026-11-21)
//! - Reference: https://distribution.github.io/distribution/spec/auth/token/ (retrieved 2026-11-21)
// TODO(images): private registry credentials (`~/.docker/config.json`) are
// deferred; this slice supports public registries and anonymous Bearer token
// exchange (such as `ghcr.io` and Docker Hub) until private feature registries
// are requested.

use std::fmt;
use std::io::{Read as _, Write as _};
use std::path::{Component, Path, PathBuf};

pub use needy_core::image::Digest;
use serde::Deserialize;

/// `Accept` header sent when pulling an OCI feature manifest.
// Source: https://github.com/opencontainers/distribution-spec/blob/v1.1.0/spec.md#pulling-manifests
// Reference: https://github.com/opencontainers/image-spec/blob/v1.1.1/manifest.md
const MANIFEST_ACCEPT: &str = "application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json";

/// Media type for a Dev Container feature tarball layer in an OCI manifest.
// Reference: https://containers.dev/implementors/features-distribution/ (retrieved 2026-11-21)
const FEATURE_LAYER_MEDIA_TYPE: &str = "application/vnd.devcontainers.layer.v1+tar";

/// Filename of the feature metadata document inside a Dev Container feature tar.
const METADATA_FILE: &str = "devcontainer-feature.json";

/// Filename of the cached feature archive inside a cache entry directory.
const ARCHIVE_FILE: &str = "feature.tar";

/// What fetching or reading a cached OCI Dev Container feature failed on.
#[derive(Debug, thiserror::Error)]
pub enum Error {
  /// The feature reference string is not a valid OCI reference.
  #[error("invalid OCI feature reference {0:?}: {1}")]
  InvalidReference(String, String),
  /// Bearer token challenge parsing or token exchange failed.
  #[error("OCI registry authentication failed: {0}")]
  Auth(String),
  /// Transport or HTTP client failure.
  #[error("HTTP request to {url} failed: {source}")]
  Http {
    url: String,
    #[source]
    source: reqwest::Error,
  },
  /// Non-2xx status from the OCI registry.
  #[error("OCI registry {url} returned HTTP {status}: {body}")]
  Registry {
    status: u16,
    url: String,
    body: String,
  },
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
  /// Local cache filesystem error.
  #[error("{context}: {source}")]
  Io {
    context: String,
    #[source]
    source: std::io::Error,
  },
}

pub type Result<T> = std::result::Result<T, Error>;

fn io(context: impl fmt::Display) -> impl FnOnce(std::io::Error) -> Error {
  move |source| Error::Io {
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
    let bad = |reason: &str| Error::InvalidReference(raw.to_string(), reason.to_string());
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
        let digest = Digest::parse(digest_part).map_err(|e| Error::InvalidReference(raw.to_string(), e.to_string()))?;
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
  /// `<registry>/<repository>`. Used as the lockfile key and for layer
  /// replacement along `extends`.
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

  fn target_segment(&self) -> String {
    match &self.target {
      RefTarget::Tag(tag) => tag.clone(),
      RefTarget::Digest(digest) => digest.to_string(),
    }
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
        .map_err(|e| Error::InvalidArchive(format!("cannot gunzip {}: {e}", self.archive_path.display())))?;
      Ok(out)
    } else {
      Ok(raw)
    }
  }
}

/// A built-in Dev Container feature bundled with needy rather than fetched
/// from an OCI registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinFeature {
  /// Stable reference name (for example `builtin:sshfs`).
  pub reference: &'static str,
  /// Raw `devcontainer-feature.json` metadata document.
  pub metadata_json: &'static str,
  /// POSIX `install.sh` script.
  pub install_sh: &'static str,
}

impl BuiltinFeature {
  /// Packs this built-in feature into an uncompressed tar archive containing
  /// `devcontainer-feature.json` and an executable `install.sh`.
  pub fn archive(&self) -> Result<Vec<u8>> {
    let mut tar = tar::Builder::new(Vec::new());
    for (name, mode, content) in [
      (METADATA_FILE, 0o644u32, self.metadata_json.as_bytes()),
      ("install.sh", 0o755u32, self.install_sh.as_bytes()),
    ] {
      let mut header = tar::Header::new_gnu();
      header.set_size(content.len() as u64);
      header.set_mode(mode);
      header.set_cksum();
      tar
        .append_data(&mut header, name, content)
        .map_err(|e| Error::InvalidArchive(format!("cannot pack built-in feature `{}`: {e}", self.reference)))?;
    }
    tar.into_inner().map_err(|e| Error::InvalidArchive(format!("cannot finalize built-in feature `{}`: {e}", self.reference)))
  }

  /// Content digest of this built-in feature's metadata and install script.
  pub fn digest(&self) -> Digest {
    let mut bytes = Vec::with_capacity(self.metadata_json.len() + self.install_sh.len() + 1);
    bytes.extend_from_slice(self.metadata_json.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(self.install_sh.as_bytes());
    Digest::of(&bytes)
  }
}

/// Embedded built-in Dev Container features shipped with needy so `builtin:<id>`
/// references work offline without an external OCI registry or repository clone.
const BUILTINS: &[BuiltinFeature] = &[
  BuiltinFeature {
    reference: "builtin:sshfs",
    metadata_json: include_str!("../../../../features/sshfs/devcontainer-feature.json"),
    install_sh: include_str!("../../../../features/sshfs/install.sh"),
  },
  BuiltinFeature {
    reference: "builtin:codex",
    metadata_json: include_str!("../../../../features/codex/devcontainer-feature.json"),
    install_sh: include_str!("../../../../features/codex/install.sh"),
  },
  BuiltinFeature {
    reference: "builtin:pi",
    metadata_json: include_str!("../../../../features/pi/devcontainer-feature.json"),
    install_sh: include_str!("../../../../features/pi/install.sh"),
  },
  BuiltinFeature {
    reference: "builtin:opencode",
    metadata_json: include_str!("../../../../features/opencode/devcontainer-feature.json"),
    install_sh: include_str!("../../../../features/opencode/install.sh"),
  },
  BuiltinFeature {
    reference: "builtin:herdr",
    metadata_json: include_str!("../../../../features/herdr/devcontainer-feature.json"),
    install_sh: include_str!("../../../../features/herdr/install.sh"),
  },
];

/// Returns whether `reference` names a built-in needy feature (`builtin:<id>`).
pub fn is_builtin_ref(reference: &str) -> bool {
  reference.starts_with("builtin:")
}

/// Returns all built-in Dev Container features embedded in needy.
pub fn all_builtins() -> &'static [BuiltinFeature] {
  BUILTINS
}

/// Looks up an embedded built-in Dev Container feature by reference (for
/// example `builtin:sshfs`, `builtin:codex`, `builtin:pi`, `builtin:opencode`,
/// `builtin:herdr`).
pub fn builtin(reference: &str) -> Option<BuiltinFeature> {
  let untagged = reference.split_once('@').map_or(reference, |(name, _)| name);
  BUILTINS.iter().find(|f| f.reference == untagged).cloned()
}

/// Needy's built-in `sshfs` Dev Container feature (decision E8 in
/// `docs/ai/references/architecture/2026-11-17-images-and-run-data-spec.md`):
/// checks `command -v sshfs` and installs `sshfs` via the root's package
/// manager when absent.
pub fn builtin_sshfs() -> BuiltinFeature {
  builtin("builtin:sshfs").expect("builtin:sshfs is embedded")
}

/// Fetches Dev Container feature artifacts over OCI Distribution v2 and caches
/// them under `cache_dir/features/sha256-<manifest-hex>/`.
#[derive(Debug, Clone)]
pub struct FeatureStore {
  cache_dir: PathBuf,
  client: reqwest::Client,
  scheme: String,
  base_url: Option<String>,
}

impl FeatureStore {
  /// Creates a new feature store caching under `cache_dir` and talking HTTPS
  /// to OCI registries.
  pub fn new(cache_dir: impl Into<PathBuf>) -> Result<Self> {
    let client =
      reqwest::Client::builder().user_agent(concat!("needy/", env!("CARGO_PKG_VERSION"))).build().map_err(|source| Error::Http {
        url: String::from("<client>"),
        source,
      })?;
    Ok(Self {
      cache_dir: cache_dir.into(),
      client,
      scheme: "https".to_string(),
      base_url: None,
    })
  }

  /// Overrides the HTTP client (for example, `no_proxy()` in loopback tests).
  pub fn with_client(mut self, client: reqwest::Client) -> Self {
    self.client = client;
    self
  }

  /// Overrides the URL scheme (`"https"` by default; `"http"` for plain HTTP
  /// test registries).
  pub fn with_scheme(mut self, scheme: impl Into<String>) -> Self {
    self.scheme = scheme.into();
    self
  }

  /// Overrides the registry base URL (for example `"http://127.0.0.1:12345"`),
  /// routing all fetches to that endpoint regardless of `reference.registry`.
  pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
    self.base_url = Some(base_url.into().trim_end_matches('/').to_string());
    self
  }

  /// Disables system HTTP proxies on the underlying client (useful for loopback
  /// test registries).
  pub fn without_proxy(mut self) -> Result<Self> {
    self.client =
      reqwest::Client::builder().user_agent(concat!("needy/", env!("CARGO_PKG_VERSION"))).no_proxy().build().map_err(|source| {
        Error::Http {
          url: String::from("<client>"),
          source,
        }
      })?;
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

  /// Fetches `reference` (or returns it from cache when pinned by digest and
  /// already present).
  pub async fn fetch(&self, reference: &Reference) -> Result<FetchedFeature> {
    if let Some(pinned) = reference.digest()
      && let Some(cached) = self.get_cached(&pinned)?
    {
      return Ok(cached);
    }

    let base = self.registry_base(reference);
    let manifest_url = format!("{base}/v2/{}/manifests/{}", reference.repository, reference.target_segment());
    let mut bearer_token: Option<String> = None;

    let manifest_resp = self.get_with_auth(&manifest_url, Some(MANIFEST_ACCEPT), &mut bearer_token).await?;
    let manifest_bytes = manifest_resp.bytes().await.map_err(|source| Error::Http {
      url: manifest_url.clone(),
      source,
    })?;
    let manifest_digest = Digest::of(&manifest_bytes);

    if let Some(expected) = reference.digest()
      && expected != manifest_digest
    {
      return Err(Error::DigestMismatch {
        what: "feature manifest",
        expected,
        actual: manifest_digest,
      });
    }

    if let Some(cached) = self.get_cached(&manifest_digest)? {
      return Ok(cached);
    }

    let manifest: OciManifest =
      serde_json::from_slice(&manifest_bytes).map_err(|e| Error::InvalidManifest(format!("cannot parse JSON: {e}")))?;
    let layer = select_feature_layer(&manifest)?;
    let layer_digest =
      Digest::parse(&layer.digest).map_err(|e| Error::InvalidManifest(format!("invalid layer digest {}: {e}", layer.digest)))?;

    let blob_url = format!("{base}/v2/{}/blobs/{layer_digest}", reference.repository);
    let blob_resp = self.get_with_auth(&blob_url, None, &mut bearer_token).await?;
    let blob_bytes = blob_resp.bytes().await.map_err(|source| Error::Http {
      url: blob_url.clone(),
      source,
    })?;

    let actual_blob_digest = Digest::of(&blob_bytes);
    if actual_blob_digest != layer_digest {
      return Err(Error::DigestMismatch {
        what: "feature blob",
        expected: layer_digest,
        actual: actual_blob_digest,
      });
    }

    let metadata_json = extract_metadata_json(&blob_bytes)?;
    self.cache_atomically(&manifest_digest, &blob_bytes, &metadata_json)
  }

  /// Materializes an embedded built-in feature (`builtin:<id>`) into the local
  /// cache with zero network requests.
  pub fn fetch_builtin(&self, reference: &str) -> Result<FetchedFeature> {
    let feature = builtin(reference).ok_or_else(|| {
      let known: Vec<&str> = BUILTINS.iter().map(|b| b.reference).collect();
      Error::InvalidReference(reference.to_string(), format!("unknown built-in feature; available: {}", known.join(", ")))
    })?;
    let digest = feature.digest();
    if let Some(cached) = self.get_cached(&digest)? {
      return Ok(cached);
    }
    let archive = feature.archive()?;
    self.cache_atomically(&digest, &archive, feature.metadata_json)
  }

  /// Fetches `reference` pinned to `pinned` (from a lockfile), hitting the
  /// local cache with zero HTTP requests when already present and otherwise
  /// pulling directly by `@sha256:...` without resolving any tag.
  pub async fn fetch_pinned(&self, reference: &Reference, pinned: &Digest) -> Result<FetchedFeature> {
    if let Some(cached) = self.get_cached(pinned)? {
      return Ok(cached);
    }
    self.fetch(&reference.with_digest(*pinned)).await
  }

  fn entry_dir(&self, digest: &Digest) -> PathBuf {
    self.cache_dir.join("features").join(format!("sha256-{}", digest.hex()))
  }

  fn registry_base(&self, reference: &Reference) -> String {
    match &self.base_url {
      Some(base) => base.clone(),
      None => format!("{}://{}", self.scheme, reference.registry),
    }
  }

  async fn get_with_auth(&self, url: &str, accept: Option<&str>, token: &mut Option<String>) -> Result<reqwest::Response> {
    let response = self.send_get(url, accept, token.as_deref()).await?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
      let header = response
        .headers()
        .get(reqwest::header::WWW_AUTHENTICATE)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| Error::Auth(format!("HTTP 401 from {url} without WWW-Authenticate header")))?;
      let challenge = parse_bearer_challenge(header)?;
      let exchanged = self.exchange_bearer_token(&challenge).await?;
      *token = Some(exchanged);
      let retry = self.send_get(url, accept, token.as_deref()).await?;
      return ensure_success(url, retry).await;
    }
    ensure_success(url, response).await
  }

  async fn send_get(&self, url: &str, accept: Option<&str>, token: Option<&str>) -> Result<reqwest::Response> {
    let mut req = self.client.get(url);
    if let Some(accept) = accept {
      req = req.header(reqwest::header::ACCEPT, accept);
    }
    if let Some(token) = token {
      req = req.bearer_auth(token);
    }
    req.send().await.map_err(|source| Error::Http {
      url: url.to_string(),
      source,
    })
  }

  async fn exchange_bearer_token(&self, challenge: &BearerChallenge) -> Result<String> {
    let mut token_url =
      reqwest::Url::parse(&challenge.realm).map_err(|e| Error::Auth(format!("invalid Bearer realm {:?}: {e}", challenge.realm)))?;
    {
      let mut pairs = token_url.query_pairs_mut();
      if let Some(service) = &challenge.service {
        pairs.append_pair("service", service);
      }
      if let Some(scope) = &challenge.scope {
        pairs.append_pair("scope", scope);
      }
    }
    let url_str = token_url.to_string();
    let resp = self.client.get(token_url).send().await.map_err(|source| Error::Http {
      url: url_str.clone(),
      source,
    })?;
    let resp = ensure_success(&url_str, resp).await?;
    let body = resp.bytes().await.map_err(|source| Error::Http {
      url: url_str.clone(),
      source,
    })?;
    let parsed: TokenResponse =
      serde_json::from_slice(&body).map_err(|e| Error::Auth(format!("cannot parse token JSON from {url_str}: {e}")))?;
    parsed
      .token
      .filter(|t| !t.is_empty())
      .or_else(|| parsed.access_token.filter(|t| !t.is_empty()))
      .ok_or_else(|| Error::Auth(format!("token response from {url_str} contained neither `token` nor `access_token`")))
  }

  fn cache_atomically(&self, digest: &Digest, blob_bytes: &[u8], metadata_json: &str) -> Result<FetchedFeature> {
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
}

fn write_and_sync(path: &Path, bytes: &[u8]) -> Result<()> {
  let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(io(path.display()))?;
  file.write_all(bytes).map_err(io(path.display()))?;
  file.sync_all().map_err(io(path.display()))?;
  Ok(())
}

async fn ensure_success(url: &str, response: reqwest::Response) -> Result<reqwest::Response> {
  let status = response.status();
  if status.is_success() {
    return Ok(response);
  }
  let body = response.text().await.unwrap_or_default();
  let snippet: String = body.chars().take(512).collect();
  Err(Error::Registry {
    status: status.as_u16(),
    url: url.to_string(),
    body: snippet,
  })
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
  Err(Error::InvalidManifest(format!(
    "expected layer with mediaType {FEATURE_LAYER_MEDIA_TYPE:?} or a single layer, found {} layers",
    manifest.layers.len()
  )))
}

#[derive(Deserialize)]
struct TokenResponse {
  token: Option<String>,
  access_token: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct BearerChallenge {
  realm: String,
  service: Option<String>,
  scope: Option<String>,
}

/// Parses an HTTP `WWW-Authenticate: Bearer ...` challenge header.
// Reference: https://distribution.github.io/distribution/spec/auth/token/ (retrieved 2026-11-21)
// Reference: RFC 6750 / RFC 7235 auth-param syntax
fn parse_bearer_challenge(header: &str) -> Result<BearerChallenge> {
  let trimmed = header.trim();
  let Some((scheme, rest)) = trimmed.split_once(char::is_whitespace) else {
    return Err(Error::Auth(format!("unsupported WWW-Authenticate header: {header:?}")));
  };
  if !scheme.eq_ignore_ascii_case("bearer") {
    return Err(Error::Auth(format!("unsupported WWW-Authenticate scheme {scheme:?}")));
  }

  let mut realm = None;
  let mut service = None;
  let mut scope = None;

  let mut chars = rest.chars().peekable();
  while chars.peek().is_some() {
    while matches!(chars.peek(), Some(c) if c.is_whitespace() || *c == ',') {
      chars.next();
    }
    if chars.peek().is_none() {
      break;
    }

    let mut key = String::new();
    while let Some(&c) = chars.peek() {
      if c == '=' || c == ',' || c.is_whitespace() {
        break;
      }
      key.push(c);
      chars.next();
    }
    while matches!(chars.peek(), Some(c) if c.is_whitespace()) {
      chars.next();
    }
    if chars.next() != Some('=') {
      return Err(Error::Auth(format!("malformed WWW-Authenticate parameter in {header:?}")));
    }
    while matches!(chars.peek(), Some(c) if c.is_whitespace()) {
      chars.next();
    }

    let value = if chars.peek() == Some(&'"') {
      chars.next();
      let mut out = String::new();
      let mut closed = false;
      while let Some(c) = chars.next() {
        if c == '\\' {
          if let Some(escaped) = chars.next() {
            out.push(escaped);
          }
        } else if c == '"' {
          closed = true;
          break;
        } else {
          out.push(c);
        }
      }
      if !closed {
        return Err(Error::Auth(format!("unterminated quoted value in WWW-Authenticate: {header:?}")));
      }
      out
    } else {
      let mut out = String::new();
      while let Some(&c) = chars.peek() {
        if c == ',' || c.is_whitespace() {
          break;
        }
        out.push(c);
        chars.next();
      }
      out
    };

    match key.to_ascii_lowercase().as_str() {
      "realm" => realm = Some(value),
      "service" => service = Some(value),
      "scope" => scope = Some(value),
      _ => {}
    }
  }

  let realm = realm.filter(|r| !r.is_empty()).ok_or_else(|| Error::Auth(format!("WWW-Authenticate Bearer missing realm: {header:?}")))?;
  Ok(BearerChallenge {
    realm,
    service,
    scope,
  })
}

/// Extracts and validates `devcontainer-feature.json` from a Dev Container
/// feature tarball (uncompressed or gzip-compressed).
fn extract_metadata_json(blob_bytes: &[u8]) -> Result<String> {
  // Gzip magic bytes (`0x1f 0x8b`, RFC 1952 section 2.3.1).
  if blob_bytes.len() >= 2 && blob_bytes[0] == 0x1f && blob_bytes[1] == 0x8b {
    extract_metadata_from_reader(flate2::read::GzDecoder::new(blob_bytes))
  } else {
    extract_metadata_from_reader(blob_bytes)
  }
}

fn extract_metadata_from_reader(reader: impl std::io::Read) -> Result<String> {
  let mut archive = tar::Archive::new(reader);
  let entries = archive.entries().map_err(|e| Error::InvalidArchive(format!("cannot read tar entries: {e}")))?;
  for entry in entries {
    let mut entry = entry.map_err(|e| Error::InvalidArchive(format!("corrupt tar entry: {e}")))?;
    let path = entry.path().map_err(|e| Error::InvalidArchive(format!("invalid tar entry path: {e}")))?;
    if is_metadata_entry(&path) {
      let mut text = String::new();
      entry.read_to_string(&mut text).map_err(|e| Error::InvalidArchive(format!("cannot read {METADATA_FILE}: {e}")))?;
      serde_json::from_str::<serde_json::Value>(&text)
        .map_err(|e| Error::InvalidArchive(format!("{METADATA_FILE} is not valid JSON: {e}")))?;
      return Ok(text);
    }
  }
  Err(Error::InvalidArchive(format!("feature archive is missing {METADATA_FILE}")))
}

fn is_metadata_entry(path: &Path) -> bool {
  let mut normal = path.components().filter(|c| !matches!(c, Component::CurDir));
  matches!(
    (normal.next(), normal.next()),
    (Some(Component::Normal(name)), None) if name == METADATA_FILE
  )
}

#[cfg(test)]
#[path = "feature_test.rs"]
mod tests;
