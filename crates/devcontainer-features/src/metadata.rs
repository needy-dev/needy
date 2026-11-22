//! Dev Container feature metadata (`devcontainer-feature.json`), JSONC parsing,
//! option schemas and resolution, and lifecycle commands.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use jsonc_parser::{ParseOptions, parse_to_serde_value};
use serde::de::{self, Visitor};
use serde::{Deserialize, Serialize};

/// Errors parsing a `devcontainer-feature.json` document.
#[derive(Debug, thiserror::Error)]
pub enum MetadataError {
  #[error("invalid JSONC in devcontainer-feature.json: {0}")]
  InvalidJsonc(String),
  #[error("invalid devcontainer-feature.json schema: {0}")]
  InvalidSchema(#[from] serde_json::Error),
}

/// A Dev Container feature option value (string, boolean, or integer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptionValue {
  String(String),
  Bool(bool),
  Int(i64),
}

impl fmt::Display for OptionValue {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::String(value) => f.write_str(value),
      Self::Bool(value) => write!(f, "{value}"),
      Self::Int(value) => write!(f, "{value}"),
    }
  }
}

impl From<&str> for OptionValue {
  fn from(value: &str) -> Self {
    Self::String(value.to_string())
  }
}

impl From<String> for OptionValue {
  fn from(value: String) -> Self {
    Self::String(value)
  }
}

impl From<bool> for OptionValue {
  fn from(value: bool) -> Self {
    Self::Bool(value)
  }
}

impl From<i64> for OptionValue {
  fn from(value: i64) -> Self {
    Self::Int(value)
  }
}

impl Serialize for OptionValue {
  fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
    match self {
      Self::String(value) => serializer.serialize_str(value),
      Self::Bool(value) => serializer.serialize_bool(*value),
      Self::Int(value) => serializer.serialize_i64(*value),
    }
  }
}

impl<'de> Deserialize<'de> for OptionValue {
  fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    deserializer.deserialize_any(OptionValueVisitor)
  }
}

struct OptionValueVisitor;

impl<'de> Visitor<'de> for OptionValueVisitor {
  type Value = OptionValue;

  fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("a string, boolean, or integer feature option value")
  }

  fn visit_str<E: de::Error>(self, value: &str) -> Result<OptionValue, E> {
    Ok(OptionValue::String(value.to_string()))
  }

  fn visit_string<E: de::Error>(self, value: String) -> Result<OptionValue, E> {
    Ok(OptionValue::String(value))
  }

  fn visit_bool<E: de::Error>(self, value: bool) -> Result<OptionValue, E> {
    Ok(OptionValue::Bool(value))
  }

  fn visit_i64<E: de::Error>(self, value: i64) -> Result<OptionValue, E> {
    Ok(OptionValue::Int(value))
  }

  fn visit_u64<E: de::Error>(self, value: u64) -> Result<OptionValue, E> {
    i64::try_from(value).map(OptionValue::Int).map_err(|_| E::custom(format!("feature option integer {value} is out of range")))
  }

  fn visit_f64<E: de::Error>(self, value: f64) -> Result<OptionValue, E> {
    Err(E::custom(format!("feature option value {value} is a floating-point number; quote decimal versions as strings (e.g. \"{value}\")")))
  }
}

/// One option entry under `options` in `devcontainer-feature.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct OptionSpec {
  /// Option type (`"string"` or `"boolean"`).
  #[serde(rename = "type", default)]
  pub kind: Option<String>,
  /// Default value when the user does not specify this option.
  #[serde(default)]
  pub default: Option<OptionValue>,
  /// Human-readable description of the option.
  #[serde(default)]
  pub description: Option<String>,
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

/// Shell-quotes `argv` into a POSIX `sh -c` command string.
pub fn shell_join(argv: &[String]) -> String {
  argv.iter().map(|arg| format!("'{}'", arg.replace('\'', "'\\''"))).collect::<Vec<_>>().join(" ")
}

/// Parsed `devcontainer-feature.json` metadata for a Dev Container feature.
// Source: https://github.com/devcontainers/spec/blob/c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421/docs/specs/devcontainer-features.md
// Reference: https://containers.dev/implementors/features/
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
  /// Environment variables added to the container in layer order.
  #[serde(rename = "containerEnv", default)]
  pub container_env: BTreeMap<String, String>,
  /// Hard dependencies that must be installed before this feature.
  #[serde(rename = "dependsOn", default)]
  pub depends_on: BTreeMap<String, BTreeMap<String, OptionValue>>,
  /// Soft ordering constraints: if any listed feature is also present, it must
  /// be installed before this feature.
  #[serde(rename = "installsAfter", default)]
  pub installs_after: Vec<String>,
  /// Whether the feature requests `--privileged`.
  #[serde(default)]
  pub privileged: bool,
  /// Linux capabilities requested by the feature (`capAdd`).
  #[serde(rename = "capAdd", default)]
  pub cap_add: Vec<String>,
  /// Security options requested by the feature (`securityOpt`).
  #[serde(rename = "securityOpt", default)]
  pub security_opt: Vec<String>,
  /// Mounts requested by the feature (`mounts`).
  #[serde(default)]
  pub mounts: Vec<serde_json::Value>,
  /// Optional entrypoint declared by the feature.
  #[serde(default)]
  pub entrypoint: Option<String>,
  /// Whether the feature requests a tiny init process.
  #[serde(default)]
  pub init: bool,
  /// Prebuild-time creation hook (`onCreateCommand`).
  #[serde(rename = "onCreateCommand", default)]
  pub on_create_command: Option<LifecycleCommand>,
  /// Content-update lifecycle hook (`updateContentCommand`).
  #[serde(rename = "updateContentCommand", default)]
  pub update_content_command: Option<LifecycleCommand>,
  /// Post-create lifecycle hook (`postCreateCommand`).
  #[serde(rename = "postCreateCommand", default)]
  pub post_create_command: Option<LifecycleCommand>,
  /// Post-start lifecycle hook (`postStartCommand`).
  #[serde(rename = "postStartCommand", default)]
  pub post_start_command: Option<LifecycleCommand>,
  /// Post-attach lifecycle hook (`postAttachCommand`).
  #[serde(rename = "postAttachCommand", default)]
  pub post_attach_command: Option<LifecycleCommand>,
  /// Tool-specific `customizations` JSON block (opaque to this crate so
  /// consumers can deserialize their own namespace under `customizations`).
  #[serde(default)]
  pub customizations: serde_json::Value,
}

impl FeatureMetadata {
  /// Parses a `devcontainer-feature.json` document (supporting JSONC comments
  /// and trailing commas).
  pub fn parse(jsonc: &str) -> Result<Self, MetadataError> {
    let options = ParseOptions {
      allow_comments: true,
      allow_trailing_commas: true,
      allow_loose_object_property_names: false,
      allow_missing_commas: false,
      allow_single_quoted_strings: false,
      allow_hexadecimal_numbers: false,
      allow_unary_plus_numbers: false,
      allow_bare_decimal_point_numbers: false,
      allow_non_finite_numbers: false,
      allow_extended_string_escapes: false,
    };
    let value = parse_to_serde_value::<Option<serde_json::Value>>(jsonc, &options)
      .map_err(|e| MetadataError::InvalidJsonc(e.to_string()))?
      .ok_or_else(|| MetadataError::InvalidJsonc("document is empty".to_string()))?;
    Ok(serde_json::from_value(value)?)
  }

  /// Resolves option defaults and user-supplied overrides into sanitized
  /// environment variables for `devcontainer-features.env`.
  pub fn resolve_options(&self, overrides: &BTreeMap<String, OptionValue>) -> BTreeMap<String, String> {
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

  /// Lifecycle commands in Dev Container start order:
  /// `updateContentCommand`, `postCreateCommand`, `postStartCommand`.
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

impl FromStr for FeatureMetadata {
  type Err = MetadataError;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    Self::parse(s)
  }
}

/// Converts a Dev Container feature option name into the shell environment
/// variable name written to `devcontainer-features.env`.
// Source: https://github.com/devcontainers/spec/blob/c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421/docs/specs/devcontainer-features.md
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

/// Returns `reference` without its `:TAG` or `@DIGEST` suffix, preserving
/// registry ports (e.g. `localhost:5000/repo:1` -> `localhost:5000/repo`) and
/// scheme-prefixed identifiers without a `/` (e.g. `builtin:sshfs`).
pub fn untagged(reference: &str) -> &str {
  let reference = reference.split_once('@').map_or(reference, |(name, _)| name);
  if reference.starts_with("builtin:") {
    return reference;
  }
  let segment = reference.rfind('/').map_or(0, |slash| slash + 1);
  match reference[segment..].rfind(':') {
    Some(colon) => &reference[..segment + colon],
    None => reference,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parses_jsonc_with_comments_and_trailing_commas() {
    let jsonc = r#"{
      // Single-line comment
      "id": "node",
      "version": "2.1.2",
      /* Multi-line
         block comment */
      "options": {
        "version": {
          "type": "string",
          "default": "lts",
        },
        "install-yarn": {
          "type": "boolean",
          "default": true,
        },
      },
      "installsAfter": [
        "ghcr.io/devcontainers/features/common-utils",
      ],
      "customizations": {
        "vscode": { "extensions": ["dbaeumer.vscode-eslint"], },
      },
    }"#;

    let meta: FeatureMetadata = jsonc.parse().unwrap();
    assert_eq!(meta.id, "node");
    assert_eq!(meta.version.as_deref(), Some("2.1.2"));
    assert_eq!(meta.installs_after, ["ghcr.io/devcontainers/features/common-utils"]);
    assert_eq!(meta.customizations["vscode"]["extensions"][0], "dbaeumer.vscode-eslint");

    let env = meta.resolve_options(&BTreeMap::from([("version".into(), OptionValue::from("22"))]));
    assert_eq!(
      env,
      BTreeMap::from([
        ("INSTALL_YARN".into(), "true".into()),
        ("VERSION".into(), "22".into()),
      ])
    );
  }

  #[test]
  fn rejects_empty_or_invalid_jsonc() {
    assert!(matches!(FeatureMetadata::parse("  // only a comment\n"), Err(MetadataError::InvalidJsonc(_))));
    assert!(FeatureMetadata::parse("{ bad json").is_err());
  }

  #[test]
  fn sanitizes_option_names_per_spec() {
    assert_eq!(sanitize_option_name("version"), "VERSION");
    assert_eq!(sanitize_option_name("install-zsh"), "INSTALL_ZSH");
    assert_eq!(sanitize_option_name("nodeGypDependencies"), "NODEGYPDEPENDENCIES");
    assert_eq!(sanitize_option_name("foo.bar/baz"), "FOO_BAR_BAZ");
    assert_eq!(sanitize_option_name("123foo"), "_FOO");
    assert_eq!(sanitize_option_name("__123_foo-bar"), "_FOO_BAR");
    assert_eq!(sanitize_option_name("_opt"), "_OPT");
    assert_eq!(sanitize_option_name("0"), "_");
  }

  #[test]
  fn untagged_strips_tags_and_digests_while_preserving_ports() {
    assert_eq!(untagged("ghcr.io/devcontainers/features/node:1"), "ghcr.io/devcontainers/features/node");
    assert_eq!(untagged("ghcr.io/devcontainers/features/node@sha256:abc"), "ghcr.io/devcontainers/features/node");
    assert_eq!(untagged("ghcr.io/devcontainers/features/node:1@sha256:abc"), "ghcr.io/devcontainers/features/node");
    assert_eq!(untagged("localhost:5000/features/node:1"), "localhost:5000/features/node");
    assert_eq!(untagged("localhost:5000/features/node"), "localhost:5000/features/node");
    assert_eq!(untagged("builtin:sshfs"), "builtin:sshfs");
  }
}
