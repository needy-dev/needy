//! The actor: the machine where the driving program runs.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::network::NetworkPolicy;
use crate::value::Words;

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Actor {
  /// A recipe reference: `builtin:NAME` or a path relative to the profile.
  pub recipe: Option<String>,
  pub limits: Limits,
  pub network: NetworkPolicy,
  /// The driving program.
  pub command: Words,
  pub env: BTreeMap<String, String>,
  pub cwd: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
  pub single_process: bool,
  pub self_replace: bool,
}
