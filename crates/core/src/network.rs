//! Network policy: what a network's gateway lets through, as the first
//! `net.request` and `net.connect` interceptor, before chaos. The gateway
//! (`needy-core-actor`'s `sidecar::gateway`) is the mechanism that runs it;
//! [`crate::intercept::net::Chain`] puts the chain in order.

use std::collections::BTreeMap;

use needy_config::profile::{ActorNetwork, service_host};
use needy_config::value::Glob;

use crate::intercept::net::{NetContext, NetDecide, NetDecision, NetOutcome};

/// A network's policy at its gateway, as the daemon resolved it from the
/// profile's `network:`.
#[derive(Debug, Clone, Default)]
pub struct Policy {
  /// Host patterns that may be reached, matched by [`Glob::matches_host`]:
  /// `*`, `*.example.com` (which also matches `example.com`), or a host, in
  /// any case.
  pub allow: Vec<Glob>,
  /// A service's host in the actor (`NAME.needy`) to the base URL its
  /// plain-HTTP requests are forwarded to. A service's host is allowed.
  pub services: BTreeMap<String, String>,
}

impl Policy {
  /// The actor gateway's policy for a profile's `actor.network`.
  pub fn actor(network: &ActorNetwork) -> Self {
    Self {
      allow: network.allow.clone(),
      services: network.services.iter().map(|(name, service)| (service_host(name), service.url.clone())).collect(),
    }
  }

  /// The base URL of the service whose host in the actor is `host`, in any
  /// case.
  pub fn service(&self, host: &str) -> Option<&str> {
    let host = host.to_ascii_lowercase();
    self.services.get(&host).map(String::as_str)
  }
}

/// The first `net.*` interceptor: a host that matches no pattern and is no
/// service is denied.
pub struct Allowlist {
  patterns: Vec<Glob>,
  services: Vec<String>,
}

impl Allowlist {
  pub fn new(policy: &Policy) -> Self {
    Self {
      patterns: policy.allow.clone(),
      services: policy.services.keys().cloned().collect(),
    }
  }

  fn allows(&self, host: &str) -> bool {
    self.patterns.iter().any(|pattern| pattern.matches_host(host)) || self.services.iter().any(|service| service.eq_ignore_ascii_case(host))
  }
}

impl NetDecide for Allowlist {
  fn decide(&self, ctx: &NetContext, mut decision: NetDecision) -> NetDecision {
    if decision.outcome == NetOutcome::Allow && !self.allows(&ctx.host) {
      decision.outcome = NetOutcome::Deny;
    }
    decision
  }
}

#[cfg(test)]
#[path = "network_test.rs"]
mod tests;
