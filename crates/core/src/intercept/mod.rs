//! Interceptors: the hook points where a flow's outcome may be changed,
//! grouped into three domains. Each domain has its context, its decision,
//! one trait for its interceptors and one function or type that assembles
//! its chain. Observers that only see outcomes are not here; they subscribe
//! to the trace.
//!
//! | Domain | Hook points | Context and decision | Trait | Chain, assembled by | Interceptors, in order | Adapter |
//! | --- | --- | --- | --- | --- | --- | --- |
//! | [`exec`] | `exec.decide` | [`exec::ExecContext`], [`exec::Decision`] | [`exec::ExecDecide`] | [`exec::Chain::new`] | [`crate::rules::Rules`], [`crate::chaos::Injector`] | `needy-daemon`'s `routing`, for the actor's proxy and `needy exec` |
//! | [`net`] | `net.request`, `net.connect` | [`net::NetContext`], [`net::NetDecision`] | [`net::NetDecide`] | [`net::Chain::new`] | [`crate::network::Allowlist`], [`crate::chaos::Injector`], [`crate::network::Inject`] | `needy-core-actor`'s `sidecar::gateway` |
//! | [`file`] | `file.open` | an exec stream's bytes, [`file::Filtered`] | [`file::StreamFilter`] | [`file::chain`] | [`crate::chaos::sftp::FileFaults`] | `needy-daemon`'s `relay`, on a workspace mount's SFTP stream |
//!
//! What differs per actor driver is only how a domain's events are caught
//! (the capture, in `needy-core-actor`'s `guest::capture`) and carried to
//! the chain (the adapter). Policy never knows the driver. See
//! `docs/ai/references/architecture/2026-11-12-drivers-and-extension-points-design.md`.
//!
//! NOTICE: interceptors are synchronous and return decisions only. Effects
//! such as a fault's delay are carried in the decision and applied by the
//! flow, so a chain is deterministic for a profile, a seed and a context, and
//! `core` needs no async runtime.

pub mod exec;
pub mod file;
pub mod net;

/// Runs a chain: each interceptor sees the decision so far and returns it,
/// changed or not. `step` asks one interceptor of the domain.
fn fold<I: ?Sized, D>(chain: &[&I], start: D, step: impl Fn(&I, D) -> D) -> D {
  chain.iter().fold(start, |decision, interceptor| step(interceptor, decision))
}
