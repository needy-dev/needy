//! Network policy: what a network's gateway lets through and adds, as the
//! `net.request` and `net.connect` interceptors around chaos. The gateway
//! (`needy-core-actor`'s `sidecar::gateway`) is the mechanism that runs
//! them; [`crate::intercept::net::Chain`] puts them in order.

use std::collections::BTreeMap;

use needy_config::value::Glob;

use crate::intercept::net::{NetContext, NetDecide, NetDecision, NetOutcome};

/// A network's policy at its gateway, as the daemon resolved it from the
/// profile's `network:`.
// NOTICE: no `Debug`: `inject` holds credentials.
#[derive(Clone, Default)]
pub struct Policy {
  /// Host patterns that may be reached, matched by [`Glob::matches_host`]:
  /// `*`, `*.example.com` (which also matches `example.com`), or a host, in
  /// any case.
  pub allow: Vec<Glob>,
  /// Host to the base URL its plain-HTTP requests are forwarded to. A mapped
  /// host is allowed.
  pub upstream: BTreeMap<String, String>,
  /// Host to the header and value added to its requests. The value is the
  /// credential itself, read by the daemon, so the actor never holds it.
  pub inject: BTreeMap<String, (String, String)>,
}

/// The first `net.*` interceptor: a host that matches no pattern and has no
/// upstream is denied.
pub struct Allowlist {
  patterns: Vec<Glob>,
  upstreams: Vec<String>,
}

impl Allowlist {
  pub fn new(policy: &Policy) -> Self {
    Self {
      patterns: policy.allow.clone(),
      upstreams: policy.upstream.keys().cloned().collect(),
    }
  }

  fn allows(&self, host: &str) -> bool {
    self.patterns.iter().any(|pattern| pattern.matches_host(host)) || self.upstreams.iter().any(|upstream| upstream == host)
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

/// The last `net.request` interceptor: adds the configured header to an
/// allowed request for its host. Tunnels are opaque and get nothing.
pub struct Inject {
  /// Host to header name and value.
  headers: BTreeMap<String, (String, String)>,
}

impl Inject {
  pub fn new(headers: BTreeMap<String, (String, String)>) -> Self {
    Self { headers }
  }
}

impl NetDecide for Inject {
  fn decide(&self, ctx: &NetContext, mut decision: NetDecision) -> NetDecision {
    if decision.outcome == NetOutcome::Allow
      && ctx.path.is_some()
      && let Some((name, value)) = self.headers.get(&ctx.host)
    {
      decision.headers.push((name.clone(), value.clone()));
    }
    decision
  }
}

#[cfg(test)]
#[path = "network_test.rs"]
mod tests;
