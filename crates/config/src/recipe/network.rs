//! Network identity: interface name, MAC address and metadata service.

use serde::Deserialize;

use crate::value::Template;

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Network {
  pub interface: Option<String>,
  /// `{seq}` derives from the session seed.
  pub mac: Option<Template>,
  pub metadata: Option<Metadata>,
}

/// The cloud metadata service the machine answers as.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Metadata {
  pub provider: Option<String>,
  /// A JSON or YAML file, relative to the recipe.
  pub file: Option<String>,
}
