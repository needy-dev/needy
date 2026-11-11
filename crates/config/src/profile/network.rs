//! Network policy for the actor gateway and the executor network.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::value::Glob;

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkPolicy {
  /// Hosts that may be reached, matched by [`Glob::matches_host`].
  pub allow: Vec<Glob>,
  /// Headers the gateway adds, by host; never visible in the actor.
  pub inject: BTreeMap<String, Injection>,
  pub record: bool,
  /// Host to upstream URL, for plain-HTTP endpoints the gateway forwards.
  pub upstream: BTreeMap<String, String>,
}

impl NetworkPolicy {
  pub fn allows(&self, host: &str) -> bool {
    self.allow.iter().any(|pattern| pattern.matches_host(host))
  }
}

/// A header that a gateway adds, with its value read from an environment
/// variable.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Injection {
  pub header: String,
  pub secret: String,
}
