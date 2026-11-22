//! OCI Dev Container feature artifact fetcher and content-addressed cache.
//!
//! Delegates remote OCI Distribution v2 fetching, authentication, and caching
//! to [`devcontainer_features::FeatureStore`] and layers needy's embedded
//! built-in features ([`crate::image::builtin`]) on top.

use std::path::PathBuf;

pub use devcontainer_features::{Digest, FetchError as Error, FetchedFeature, RefTarget, Reference, canonical_ref};

pub use crate::image::builtin::{BuiltinFeature, all_builtins, builtin, builtin_sshfs, is_builtin_ref};

pub type Result<T> = std::result::Result<T, Error>;

/// Fetches Dev Container feature artifacts over OCI Distribution v2 (via
/// [`devcontainer_features::FeatureStore`]) and materializes needy's embedded
/// `builtin:<id>` features into the same content-addressed cache.
#[derive(Debug, Clone)]
pub struct FeatureStore(devcontainer_features::FeatureStore);

impl FeatureStore {
  /// Creates a new feature store caching under `cache_dir` and talking HTTPS
  /// to OCI registries.
  pub fn new(cache_dir: impl Into<PathBuf>) -> Result<Self> {
    Ok(Self(devcontainer_features::FeatureStore::new(cache_dir)?))
  }

  /// Overrides the URL scheme (`"https"` by default; `"http"` for plain HTTP
  /// test registries).
  pub fn with_scheme(self, scheme: impl Into<String>) -> Self {
    Self(self.0.with_scheme(scheme))
  }

  /// Overrides the registry base URL (for example `"http://127.0.0.1:12345"`),
  /// routing all fetches to that endpoint regardless of `reference.registry`.
  pub fn with_base_url(self, base_url: impl Into<String>) -> Self {
    Self(self.0.with_base_url(base_url))
  }

  /// Disables HTTP proxies for loopback test registries.
  pub fn without_proxy(self) -> Result<Self> {
    Ok(Self(self.0.without_proxy()?))
  }

  /// Looks up a feature already cached under `digest`, making no network calls.
  pub fn get_cached(&self, digest: &Digest) -> Result<Option<FetchedFeature>> {
    self.0.get_cached(digest)
  }

  /// Fetches `reference` (or returns it from cache when pinned by digest and
  /// already present). Concurrent fetches for the same reference share one
  /// in-flight download.
  pub async fn fetch(&self, reference: &Reference) -> Result<FetchedFeature> {
    self.0.fetch(reference).await
  }

  /// Fetches `reference` pinned to `pinned` (from a lockfile), hitting the
  /// local cache with zero HTTP requests when already present and otherwise
  /// pulling directly by `@sha256:...` without resolving any tag.
  pub async fn fetch_pinned(&self, reference: &Reference, pinned: &Digest) -> Result<FetchedFeature> {
    self.0.fetch_pinned(reference, pinned).await
  }

  /// Materializes an embedded built-in feature (`builtin:<id>`) into the local
  /// cache with zero network requests.
  pub fn fetch_builtin(&self, reference: &str) -> Result<FetchedFeature> {
    let feature = builtin(reference).ok_or_else(|| {
      let known: Vec<&str> = all_builtins().iter().map(|b| b.reference).collect();
      Error::InvalidReference(reference.to_string(), format!("unknown built-in feature; available: {}", known.join(", ")))
    })?;
    let digest = feature.digest();
    if let Some(cached) = self.get_cached(&digest)? {
      return Ok(cached);
    }
    let archive = feature.archive()?;
    self.0.cache_atomically(&digest, &archive, feature.metadata_json)
  }
}

#[cfg(test)]
#[path = "feature_test.rs"]
mod tests;
