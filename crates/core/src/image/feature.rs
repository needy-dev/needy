//! Dev Container feature metadata (`devcontainer-feature.json`), option
//! resolution, and security refusals. Delegates generic Dev Container feature
//! parsing and option resolution to `devcontainer-features` while keeping
//! needy-specific `customizations.needy` sync mappings and E3 security refusals
//! in `needy-core`.
//!
//! See "Resolving and planning a build" in
//! `docs/ai/references/architecture/2026-11-17-images-and-run-data-spec.md`.

use std::collections::BTreeMap;
use std::ops::Deref;
use std::path::{Component, Path};
use std::str::FromStr;

pub use devcontainer_features::{LifecycleCommand, LifecycleEntry, MetadataError, OptionSpec, OptionValue, sanitize_option_name};
use needy_config::image::FeatureOption;
use needy_config::profile::{OnConflict, deserialize_on_conflict};
use serde::Deserialize;

/// Errors parsing or validating a Dev Container feature's metadata.
#[derive(Debug, thiserror::Error)]
pub enum FeatureError {
  #[error("invalid devcontainer-feature.json: {0}")]
  InvalidMetadata(#[from] MetadataError),
  #[error("invalid customizations.needy in devcontainer-feature.json: {0}")]
  InvalidCustomizations(#[from] serde_json::Error),
  #[error("feature `{feature}` is refused because it sets `{property}`: {reason}")]
  Refused {
    feature: String,
    property: &'static str,
    reason: &'static str,
  },
  #[error("feature `{feature}` has invalid sync mapping `{name}` in customizations.needy.files: {reason}")]
  InvalidSyncFile {
    feature: String,
    name: String,
    reason: String,
  },
}

/// Converts a needy profile/image `FeatureOption` into a standalone
/// `devcontainer_features::OptionValue`.
pub fn to_option_value(opt: &FeatureOption) -> OptionValue {
  match opt {
    FeatureOption::String(s) => OptionValue::String(s.clone()),
    FeatureOption::Bool(b) => OptionValue::Bool(*b),
    FeatureOption::Int(i) => OptionValue::Int(*i),
  }
}

/// Parsed `devcontainer-feature.json` metadata for a Dev Container feature,
/// pairing the standalone [`devcontainer_features::FeatureMetadata`] with
/// needy-specific [`FeatureCustomizations`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeatureMetadata {
  /// Generic Dev Container feature metadata parsed by `devcontainer-features`.
  pub inner: devcontainer_features::FeatureMetadata,
  /// Needy-specific `customizations.needy` block.
  pub customizations: FeatureCustomizations,
}

impl Deref for FeatureMetadata {
  type Target = devcontainer_features::FeatureMetadata;

  fn deref(&self) -> &Self::Target {
    &self.inner
  }
}

/// Tool-specific `customizations` block in `devcontainer-feature.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct FeatureCustomizations {
  /// Needy-specific customizations (`customizations.needy`).
  #[serde(default)]
  pub needy: Option<NeedyCustomizations>,
}

/// Needy-specific customizations under `customizations.needy` in
/// `devcontainer-feature.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NeedyCustomizations {
  /// Sync mappings declared by this feature, keyed by mapping name.
  /// By convention, each mapping's store lives at `<data_dir>/files/<store>`
  /// (where `<store>` defaults to the mapping name).
  #[serde(default)]
  pub files: BTreeMap<String, FeatureFileSpec>,
}

/// One sync mapping entry under `customizations.needy.files.<name>` in
/// `devcontainer-feature.json`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeatureFileSpec {
  /// Target path inside the actor (e.g. `"~/.opencode"` or `"/home/agent/.opencode"`).
  pub path: String,
  /// Optional store key under `<data_dir>/files/<store>`; defaults to the
  /// mapping name when omitted.
  #[serde(default)]
  pub store: Option<String>,
  /// rclone-style ordered `+` and `-` rules; first match wins.
  #[serde(default)]
  pub filter: Vec<String>,
  /// Conflict resolution choices for entries changed by both the run and the
  /// store, in the order written: the first matching pattern applies.
  #[serde(
    default,
    rename = "on-conflict",
    deserialize_with = "deserialize_on_conflict"
  )]
  pub on_conflict: Vec<(String, OnConflict)>,
}

impl FeatureMetadata {
  /// Parses a `devcontainer-feature.json` document (including JSONC comments
  /// and trailing commas).
  pub fn parse(jsonc: &str) -> Result<Self, FeatureError> {
    let inner = devcontainer_features::FeatureMetadata::parse(jsonc)?;
    let needy = match inner.customizations.get("needy") {
      Some(value) if !value.is_null() => Some(serde_json::from_value::<NeedyCustomizations>(value.clone())?),
      _ => None,
    };
    Ok(Self {
      inner,
      customizations: FeatureCustomizations { needy },
    })
  }

  /// Checks decision E3 of the images and run data spec: refuses features that
  /// request `privileged`, `capAdd`, `securityOpt`, `mounts`, or `entrypoint`.
  /// `init` is ignored.
  pub fn check_allowed(&self, reference: &str) -> Result<(), FeatureError> {
    if self.inner.privileged {
      return Err(FeatureError::Refused {
        feature: reference.to_string(),
        property: "privileged",
        reason: "an image definition cannot widen its actor's privileges",
      });
    }
    if !self.inner.cap_add.is_empty() {
      return Err(FeatureError::Refused {
        feature: reference.to_string(),
        property: "capAdd",
        reason: "an image definition cannot add Linux capabilities to its actor",
      });
    }
    if !self.inner.security_opt.is_empty() {
      return Err(FeatureError::Refused {
        feature: reference.to_string(),
        property: "securityOpt",
        reason: "an image definition cannot alter its actor's security options",
      });
    }
    if !self.inner.mounts.is_empty() {
      return Err(FeatureError::Refused {
        feature: reference.to_string(),
        property: "mounts",
        reason: "an image definition cannot declare host or volume mounts",
      });
    }
    if self.inner.entrypoint.as_deref().is_some_and(|e| !e.is_empty()) {
      return Err(FeatureError::Refused {
        feature: reference.to_string(),
        property: "entrypoint",
        reason: "feature entrypoints are not supported in needy actors",
      });
    }
    if let Some(needy) = &self.customizations.needy {
      for (name, spec) in &needy.files {
        validate_sync_file(reference, name, spec)?;
      }
    }
    Ok(())
  }

  /// Resolves option defaults and user-supplied `with:` values into sanitized
  /// environment variables for `devcontainer-features.env`.
  pub fn resolve_options(&self, overrides: &BTreeMap<String, FeatureOption>) -> BTreeMap<String, String> {
    let converted: BTreeMap<String, OptionValue> = overrides.iter().map(|(k, v)| (k.clone(), to_option_value(v))).collect();
    self.inner.resolve_options(&converted)
  }

  /// Lifecycle commands mapped to needy's `start` hook, in Dev Container
  /// lifecycle order: `updateContentCommand`, `postCreateCommand`,
  /// `postStartCommand`.
  pub fn start_hooks(&self) -> Vec<String> {
    self.inner.start_hooks()
  }
}

impl FromStr for FeatureMetadata {
  type Err = FeatureError;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    Self::parse(s)
  }
}

/// Validates a single `customizations.needy.files.<name>` entry: safe mapping
/// name, safe store key under `<data_dir>/files/<store>` (no path traversal or
/// absolute escape), non-empty actor path rooted at `/` or `~`, and non-empty
/// `on-conflict` patterns.
fn validate_sync_file(reference: &str, name: &str, spec: &FeatureFileSpec) -> Result<(), FeatureError> {
  let err = |reason: String| FeatureError::InvalidSyncFile {
    feature: reference.to_string(),
    name: name.to_string(),
    reason,
  };
  if !is_single_safe_segment(name) {
    return Err(err(format!("mapping name {name:?} must be a single safe path component without `/`, `\\`, `.`, `..` or leading `~`")));
  }
  if let Some(store) = &spec.store
    && !is_safe_store_key(store)
  {
    return Err(err(format!(
      "store key {store:?} must be a relative path under needy's `files/` directory without `..`, `.`, leading `/` or `~`"
    )));
  }
  let path = spec.path.trim();
  if path.is_empty() || path.contains('\0') {
    return Err(err("actor `path` must be non-empty".to_string()));
  }
  if !path.starts_with('/') && path != "~" && !path.starts_with("~/") {
    return Err(err(format!("actor `path` {path:?} must be an absolute path or start with `~/`")));
  }
  for (pattern, _) in &spec.on_conflict {
    if pattern.trim().is_empty() {
      return Err(err("`on-conflict` pattern must be non-empty".to_string()));
    }
  }
  Ok(())
}

/// Returns true when `name` is a single normal path segment suitable for a
/// per-run sync directory name (`sync/runs/<run>/<name>`).
fn is_single_safe_segment(name: &str) -> bool {
  if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains('\0') || name.starts_with('~') {
    return false;
  }
  let mut comps = Path::new(name).components();
  matches!((comps.next(), comps.next()), (Some(Component::Normal(_)), None))
}

/// Returns true when `store` is a non-empty relative path of normal components
/// that stays strictly under `<data_dir>/files/`.
fn is_safe_store_key(store: &str) -> bool {
  if store.is_empty() || store.contains('\\') || store.contains('\0') || store.starts_with('~') {
    return false;
  }
  let mut has_component = false;
  for comp in Path::new(store).components() {
    match comp {
      Component::Normal(_) => has_component = true,
      _ => return false,
    }
  }
  has_component
}

#[cfg(test)]
#[path = "feature_test.rs"]
mod tests;
