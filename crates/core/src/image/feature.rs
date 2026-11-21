//! Dev Container feature metadata (`devcontainer-feature.json`), option
//! resolution, and security refusals. No IO: fetching feature artifacts and
//! reading `devcontainer-feature.json` from disk or a registry belong to
//! `needy-core-actor`.
//!
//! See "Resolving and planning a build" in
//! `docs/ai/references/architecture/2026-11-17-images-and-run-data-spec.md`.

use std::collections::BTreeMap;

use needy_config::image::FeatureOption;
use serde::Deserialize;

/// Errors parsing or validating a Dev Container feature's metadata.
#[derive(Debug, thiserror::Error)]
pub enum FeatureError {
  #[error("invalid devcontainer-feature.json: {0}")]
  InvalidMetadata(#[from] serde_json::Error),
  #[error("feature `{feature}` is refused because it sets `{property}`: {reason}")]
  Refused {
    feature: String,
    property: &'static str,
    reason: &'static str,
  },
}

/// Parsed `devcontainer-feature.json` metadata for a Dev Container feature.
// Source: https://github.com/devcontainers/spec/blob/c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421/docs/specs/devcontainer-features.md
// (retrieved 2026-11-16; see `docs/ai/references/architecture/2026-11-16-environments-and-snapshots-design.md#sources`).
// TODO(images): lifecycle hooks (`onCreateCommand`, `updateContentCommand`,
// `postCreateCommand`, `postStartCommand`, `postAttachCommand`) come with
// slice 4 of the images and run data spec.
// TODO(images): `customizations.needy.files` (feature-declared sync mappings)
// comes with M4 of the images and run data spec.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct FeatureMetadata {
  /// Feature identifier (required by the Dev Container Features spec).
  pub id: String,
  /// Semantic version of the feature, if declared.
  #[serde(default)]
  pub version: Option<String>,
  /// Human-readable display name.
  #[serde(default)]
  pub name: Option<String>,
  /// Declared option schemas and defaults.
  #[serde(default)]
  pub options: BTreeMap<String, OptionSpec>,
  /// Environment variables added to the image in layer order.
  #[serde(rename = "containerEnv", default)]
  pub container_env: BTreeMap<String, String>,
  /// Hard dependencies that must be installed before this feature.
  #[serde(rename = "dependsOn", default)]
  pub depends_on: BTreeMap<String, BTreeMap<String, FeatureOption>>,
  /// Soft ordering constraints: if any listed feature is also present, it must
  /// appear before this feature.
  #[serde(rename = "installsAfter", default)]
  pub installs_after: Vec<String>,
  /// Refused when `true` (E3: an image cannot widen its actor).
  #[serde(default)]
  pub privileged: bool,
  /// Refused when non-empty (E3).
  #[serde(rename = "capAdd", default)]
  pub cap_add: Vec<String>,
  /// Refused when non-empty (E3).
  #[serde(rename = "securityOpt", default)]
  pub security_opt: Vec<String>,
  /// Refused when non-empty (E3).
  #[serde(default)]
  pub mounts: Vec<serde_json::Value>,
  /// Refused when non-empty (E3: feature entrypoints are not supported).
  #[serde(default)]
  pub entrypoint: Option<String>,
  /// Ignored: needy's own init is PID 1 in every actor.
  #[serde(default)]
  pub init: bool,
}

/// One option entry under `options` in `devcontainer-feature.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct OptionSpec {
  /// Option type (`"string"` or `"boolean"`).
  #[serde(rename = "type", default)]
  pub kind: Option<String>,
  /// Default value when the user does not set this option under `with:`.
  #[serde(default)]
  pub default: Option<FeatureOption>,
  /// Human-readable description of the option.
  #[serde(default)]
  pub description: Option<String>,
}

impl FeatureMetadata {
  /// Parses a `devcontainer-feature.json` document.
  pub fn parse(json: &str) -> Result<Self, FeatureError> {
    Ok(serde_json::from_str(json)?)
  }

  /// Checks decision E3 of the images and run data spec: refuses features that
  /// request `privileged`, `capAdd`, `securityOpt`, `mounts`, or `entrypoint`.
  /// `init` is ignored.
  pub fn check_allowed(&self, reference: &str) -> Result<(), FeatureError> {
    if self.privileged {
      return Err(FeatureError::Refused {
        feature: reference.to_string(),
        property: "privileged",
        reason: "an image definition cannot widen its actor's privileges",
      });
    }
    if !self.cap_add.is_empty() {
      return Err(FeatureError::Refused {
        feature: reference.to_string(),
        property: "capAdd",
        reason: "an image definition cannot add Linux capabilities to its actor",
      });
    }
    if !self.security_opt.is_empty() {
      return Err(FeatureError::Refused {
        feature: reference.to_string(),
        property: "securityOpt",
        reason: "an image definition cannot alter its actor's security options",
      });
    }
    if !self.mounts.is_empty() {
      return Err(FeatureError::Refused {
        feature: reference.to_string(),
        property: "mounts",
        reason: "an image definition cannot declare host or volume mounts",
      });
    }
    if self.entrypoint.as_deref().is_some_and(|e| !e.is_empty()) {
      return Err(FeatureError::Refused {
        feature: reference.to_string(),
        property: "entrypoint",
        reason: "feature entrypoints are not supported in needy actors",
      });
    }
    Ok(())
  }

  /// Resolves option defaults and user-supplied `with:` values into sanitized
  /// environment variables for `devcontainer-features.env`.
  pub fn resolve_options(&self, overrides: &BTreeMap<String, FeatureOption>) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for (name, spec) in &self.options {
      if let Some(default) = &spec.default {
        env.insert(sanitize_option_name(name), default.to_string());
      }
    }
    for (name, value) in overrides {
      env.insert(sanitize_option_name(name), value.to_string());
    }
    env
  }
}

/// Converts a Dev Container feature option name into the shell environment
/// variable name written to `devcontainer-features.env`.
// Source: https://github.com/devcontainers/spec/blob/c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421/docs/specs/devcontainer-features.md
// (retrieved 2026-11-16; see `docs/ai/references/architecture/2026-11-16-environments-and-snapshots-design.md#sources`).
// Reference: Dev Container Features specification, "Option Resolution":
// `str.replace(/[^\w_]/g, '_').replace(/^[\d_]+/g, '_').toUpperCase()`
pub fn sanitize_option_name(name: &str) -> String {
  let replaced: String = name
    .chars()
    .map(|c| {
      if c.is_ascii_alphanumeric() || c == '_' {
        c
      } else {
        '_'
      }
    })
    .collect();
  let trimmed = replaced.trim_start_matches(|c: char| c.is_ascii_digit() || c == '_');
  let collapsed = if trimmed.len() < replaced.len() {
    format!("_{trimmed}")
  } else {
    replaced
  };
  collapsed.to_ascii_uppercase()
}

#[cfg(test)]
#[path = "feature_test.rs"]
mod tests;
