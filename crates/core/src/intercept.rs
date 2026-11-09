//! Interceptors: the hook points where a flow's outcome may be changed, one
//! trait per point. Observers that only see outcomes are not here; they
//! subscribe to the trace.
//!
//! | Point | Interceptors, in order |
//! | --- | --- |
//! | `exec.decide` | [`crate::rules::Rules`], then [`crate::chaos::Injector`] |
//! | `net.request`, `net.connect` | the gateway's allowlist, then [`crate::chaos::Injector`], then the gateway's header injection |
//! | `exec.output` | the stream filters the daemon's relay installs per stream: the SFTP fault tap ([`crate::chaos::sftp::SftpTap`]) |
//!
//! NOTICE: interceptors are synchronous and return decisions only. Effects
//! such as a fault's delay are carried in the decision and applied by the
//! flow, so a chain is deterministic for a profile, a seed and a context, and
//! `core` needs no async runtime.

use std::collections::BTreeMap;

use crate::types::{Decision, Fired, NetDecision, Network, Outcome, Placement};

/// What needy knows about a command when it decides.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecContext {
  pub argv: Vec<String>,
  pub cwd: String,
  pub env: BTreeMap<String, String>,
  pub tty: bool,
  /// Argument vectors of ancestor processes, nearest first. The actor's
  /// proxy sends them in `Decide`; other execs have none.
  pub ancestors: Vec<Vec<String>>,
}

/// An `exec.decide` interceptor: it sees the decision so far and returns it,
/// changed or not.
pub trait ExecDecide {
  fn decide(&self, ctx: &ExecContext, decision: Decision) -> Decision;
}

impl Decision {
  /// The command as given, placed at `placement`, before any interceptor.
  pub fn unchanged(ctx: &ExecContext, placement: Placement) -> Decision {
    Decision {
      outcome: Outcome::Run(placement),
      argv: ctx.argv.clone(),
      env: ctx.env.clone(),
      clear_env: false,
      cwd: ctx.cwd.clone(),
      files: Vec::new(),
      faults: Vec::new(),
      timeout: None,
      matched: Vec::new(),
      effective: ctx.argv.clone(),
      fired: Vec::new(),
    }
  }
}

/// Runs an `exec.decide` chain from `start`.
// TODO(intercept): a user decider after chaos (the `ask` action in the
// profile note) is deferred until the owner approves its profile syntax; it
// would be recorded for replay and off by default.
pub fn decide(chain: &[&dyn ExecDecide], ctx: &ExecContext, start: Decision) -> Decision {
  chain.iter().fold(start, |decision, interceptor| interceptor.decide(ctx, decision))
}

/// What needy knows about a request or tunnel at a gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetContext {
  pub network: Network,
  pub host: String,
  /// The request's path at `net.request`; `None` at `net.connect`, whose
  /// tunnel is opaque.
  pub path: Option<String>,
}

/// A `net.request` and `net.connect` interceptor: it sees the decision so
/// far and returns it, changed or not.
// NOTICE: the two points share one trait because they share their
// interceptors and context; `NetContext::path` tells them apart.
pub trait NetDecide {
  fn decide(&self, ctx: &NetContext, decision: NetDecision) -> NetDecision;
}

/// Runs a `net.request` or `net.connect` chain from `start`.
pub fn decide_net(chain: &[&dyn NetDecide], ctx: &NetContext, start: NetDecision) -> NetDecision {
  chain.iter().fold(start, |decision, interceptor| interceptor.decide(ctx, decision))
}

/// An `exec.output` interceptor: a filter on one exec stream's bytes, in
/// both directions. It may pass bytes on, hold them until a whole message
/// has arrived, or answer the caller itself.
pub trait StreamFilter: Send + Sync {
  /// Bytes the caller sends the process.
  fn inbound(&self, bytes: &[u8]) -> Filtered;
  /// Bytes the process sends the caller; what it returns goes on.
  fn outbound(&self, bytes: &[u8]) -> Vec<u8>;
}

/// What a [`StreamFilter`] makes of the caller's bytes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Filtered {
  /// Bytes for the process.
  pub forward: Vec<u8>,
  /// Answers for the caller, in order, which the process never sees.
  pub replies: Vec<Vec<u8>>,
  /// Faults the filter injected.
  pub fired: Vec<Fired>,
}
