//! The net domain: `net.request` and `net.connect`, whether a request or
//! tunnel from the actor's network goes through, and what it carries. The
//! gateway asks it for every request and tunnel, the same for every actor
//! driver.

use std::fmt;
use std::sync::Arc;

use crate::chaos::Injector;
use crate::network::{Allowlist, Inject, Policy};
use crate::types::{Fired, Network};

/// What needy knows about a request or tunnel at a gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetContext {
  pub network: Network,
  pub host: String,
  /// The request's path at `net.request`; `None` at `net.connect`, whose
  /// tunnel is opaque.
  pub path: Option<String>,
}

/// The `net.request` or `net.connect` result: whether a gateway lets a
/// request or tunnel through, and what it adds.
// NOTICE: `headers` may hold an injected credential; do not log a decision.
#[derive(Clone, PartialEq, Eq)]
pub struct NetDecision {
  pub outcome: NetOutcome,
  /// Headers set on the upstream request, as name and value. A `net.connect`
  /// tunnel is opaque, so nothing adds headers to it.
  pub headers: Vec<(String, String)>,
  /// Faults that chaos injected. The flow applies their delays; an errno is
  /// already in `outcome`.
  pub fired: Vec<Fired>,
}

impl NetDecision {
  /// Allowed and unchanged, before any interceptor.
  pub fn allowed() -> NetDecision {
    NetDecision {
      outcome: NetOutcome::Allow,
      headers: Vec::new(),
      fired: Vec::new(),
    }
  }
}

impl fmt::Debug for NetDecision {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let names: Vec<&str> = self.headers.iter().map(|(name, _)| name.as_str()).collect();
    f.debug_struct("NetDecision").field("outcome", &self.outcome).field("headers", &names).field("fired", &self.fired).finish()
  }
}

/// What happens to a request or tunnel at a gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetOutcome {
  Allow,
  /// The network policy does not allow the host.
  Deny,
  /// An injected fault failed it with this errno.
  Fault(String),
}

/// A `net.request` and `net.connect` interceptor: it sees the decision so
/// far and returns it, changed or not.
// NOTICE: the two points share one trait because they share their
// interceptors and context; `NetContext::path` tells them apart.
pub trait NetDecide {
  fn decide(&self, ctx: &NetContext, decision: NetDecision) -> NetDecision;
}

/// Runs a `net.request` or `net.connect` chain from `start`.
pub fn decide(chain: &[&dyn NetDecide], ctx: &NetContext, start: NetDecision) -> NetDecision {
  super::fold(chain, start, |interceptor, decision| interceptor.decide(ctx, decision))
}

/// One network's `net.request` and `net.connect` chain: the allowlist,
/// chaos, then header injection.
// NOTICE: no `Debug`: `inject` holds credentials.
pub struct Chain {
  allowlist: Allowlist,
  injector: Option<Arc<Injector>>,
  inject: Inject,
}

impl Chain {
  /// The chain for a network with `policy`, and the run's faults in
  /// `injector` when it has any.
  pub fn new(policy: &Policy, injector: Option<Arc<Injector>>) -> Self {
    Self {
      allowlist: Allowlist::new(policy),
      injector,
      inject: Inject::new(policy.inject.clone()),
    }
  }

  /// Decides the request or tunnel `ctx`, from allowed and unchanged.
  pub fn decide(&self, ctx: &NetContext) -> NetDecision {
    let mut chain: Vec<&dyn NetDecide> = vec![&self.allowlist];
    if let Some(injector) = &self.injector {
      chain.push(injector.as_ref());
    }
    chain.push(&self.inject);
    decide(&chain, ctx, NetDecision::allowed())
  }
}
