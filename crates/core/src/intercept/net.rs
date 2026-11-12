//! The net domain: `net.request` and `net.connect`, whether a request or
//! tunnel from the actor's network goes through. The gateway asks it for
//! every request and tunnel, the same for every actor driver.

use std::sync::Arc;

use crate::chaos::Injector;
use crate::network::{Allowlist, Policy};
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
/// request or tunnel through.
// TODO(mitm): header injection (a `headers` field set by an interceptor
// after chaos) was removed on 2026-11-12: on HTTPS it needs a separate MITM
// infrastructure (its own CA, opt-in, per host); deferred until that exists
// and the owner approves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetDecision {
  pub outcome: NetOutcome,
  /// Faults that chaos injected. The flow applies their delays and the
  /// effect of the one in `outcome`.
  pub fired: Vec<Fired>,
}

impl NetDecision {
  /// Allowed, before any interceptor.
  pub fn allowed() -> NetDecision {
    NetDecision {
      outcome: NetOutcome::Allow,
      fired: Vec::new(),
    }
  }

  /// The fault that failed the request or tunnel, if one did.
  pub fn fault(&self) -> Option<&Fired> {
    match &self.outcome {
      NetOutcome::Fault(name) => self.fired.iter().find(|fired| &fired.name == name),
      _ => None,
    }
  }
}

/// What happens to a request or tunnel at a gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetOutcome {
  Allow,
  /// The network policy does not allow the host.
  Deny,
  /// The chaos fault of this name fails it.
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

/// One network's `net.request` and `net.connect` chain: the allowlist, then
/// chaos.
pub struct Chain {
  allowlist: Allowlist,
  injector: Option<Arc<Injector>>,
}

impl Chain {
  /// The chain for a network with `policy`, and the run's faults in
  /// `injector` when it has any.
  pub fn new(policy: &Policy, injector: Option<Arc<Injector>>) -> Self {
    Self {
      allowlist: Allowlist::new(policy),
      injector,
    }
  }

  /// Decides the request or tunnel `ctx`, from allowed.
  pub fn decide(&self, ctx: &NetContext) -> NetDecision {
    let mut chain: Vec<&dyn NetDecide> = vec![&self.allowlist];
    if let Some(injector) = &self.injector {
      chain.push(injector.as_ref());
    }
    decide(&chain, ctx, NetDecision::allowed())
  }
}
