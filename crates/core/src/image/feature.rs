//! Dev Container feature metadata (`devcontainer-feature.json`), option
//! resolution, and security refusals. No IO: fetching feature artifacts and
//! reading `devcontainer-feature.json` from disk or a registry belong to
//! `needy-core-actor`.
//!
//! See "Resolving and planning a build" in
//! `docs/ai/references/architecture/2026-11-17-images-and-run-data-spec.md`.

use std::collections::BTreeMap;
use std::path::{Component, Path};

use needy_config::image::{FeatureOption, shell_join};
use needy_config::profile::{OnConflict, deserialize_on_conflict};
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
  #[error("feature `{feature}` has invalid sync mapping `{name}` in customizations.needy.files: {reason}")]
  InvalidSyncFile {
    feature: String,
    name: String,
    reason: String,
  },
}

/// Parsed `devcontainer-feature.json` metadata for a Dev Container feature.
// Source: https://github.com/devcontainers/spec/blob/c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421/docs/specs/devcontainer-features.md
// (retrieved 2026-11-16; see `docs/ai/references/architecture/2026-11-16-environments-and-snapshots-design.md#sources`).
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
  /// Prebuild-time creation hook (`onCreateCommand`).
  // TODO(images): `onCreateCommand` is a prebuild-time hook (analogue to a
  // `run` layer in `2026-11-16-environments-and-snapshots-design.md`); running
  // it as a cached build step is deferred until a feature requires it.
  #[serde(rename = "onCreateCommand", default)]
  pub on_create_command: Option<LifecycleCommand>,
  /// Mapped to needy's `start` hook (runs before `postCreateCommand` and
  /// `postStartCommand`).
  #[serde(rename = "updateContentCommand", default)]
  pub update_content_command: Option<LifecycleCommand>,
  /// Mapped to needy's `start` hook.
  #[serde(rename = "postCreateCommand", default)]
  pub post_create_command: Option<LifecycleCommand>,
  /// Mapped to needy's `start` hook.
  #[serde(rename = "postStartCommand", default)]
  pub post_start_command: Option<LifecycleCommand>,
  /// Attach-time hook (`postAttachCommand`).
  // TODO(images): `attach` hook is deferred until a case needs it, see
  // `docs/ai/references/architecture/2026-11-17-images-and-run-data-spec.md`.
  #[serde(rename = "postAttachCommand", default)]
  pub post_attach_command: Option<LifecycleCommand>,
  /// Tool-specific customizations (`customizations.needy.files` declares sync
  /// mappings for the feature; other tools' keys are ignored).
  #[serde(default)]
  pub customizations: FeatureCustomizations,
}

/// A Dev Container lifecycle command in `devcontainer-feature.json`: a shell
/// string, an argv array, or an object map of named commands (each a string or
/// argv array).
// Source: https://github.com/devcontainers/spec/blob/c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421/docs/specs/devcontainer-features.md
// Reference: https://github.com/devcontainers/spec/blob/c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421/docs/specs/devcontainerjson-reference.md#lifecycle-scripts
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum LifecycleCommand {
  /// A shell command string executed via `sh -c`.
  Shell(String),
  /// An argv array executed directly (shell-quoted for `sh -c`).
  Argv(Vec<String>),
  /// Named commands; flattened in deterministic key order.
  // NOTICE: Dev Container specifies parallel execution for object-form hooks;
  // needy runs hooks sequentially in sorted key order for deterministic logs
  // and routing traces.
  Map(BTreeMap<String, LifecycleEntry>),
}

/// One value in an object-form Dev Container lifecycle command.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum LifecycleEntry {
  Shell(String),
  Argv(Vec<String>),
}

impl LifecycleEntry {
  fn to_shell(&self) -> Option<String> {
    match self {
      Self::Shell(cmd) if !cmd.is_empty() => Some(cmd.clone()),
      Self::Shell(_) => None,
      Self::Argv(argv) if !argv.is_empty() => Some(shell_join(argv)),
      Self::Argv(_) => None,
    }
  }
}

impl LifecycleCommand {
  /// Converts this lifecycle command into zero or more POSIX `sh -c` command
  /// strings in deterministic order.
  pub fn to_shell_commands(&self) -> Vec<String> {
    match self {
      Self::Shell(cmd) => (!cmd.is_empty()).then(|| cmd.clone()).into_iter().collect(),
      Self::Argv(argv) => (!argv.is_empty()).then(|| shell_join(argv)).into_iter().collect(),
      Self::Map(entries) => entries.values().filter_map(LifecycleEntry::to_shell).collect(),
    }
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

  /// Lifecycle commands mapped to needy's `start` hook, in Dev Container
  /// lifecycle order: `updateContentCommand`, `postCreateCommand`,
  /// `postStartCommand`.
  pub fn start_hooks(&self) -> Vec<String> {
    let mut hooks = Vec::new();
    for cmd in [
      &self.update_content_command,
      &self.post_create_command,
      &self.post_start_command,
    ]
    .into_iter()
    .flatten()
    {
      hooks.extend(cmd.to_shell_commands());
    }
    hooks
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
