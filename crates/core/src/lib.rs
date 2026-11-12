//! Needy's domain logic, shared by the daemon, actors, executors and the CLI.
//! Profiles and recipes are loaded by `needy-config`; this crate interprets
//! them.
//!
//! - [`rules`] and [`recipe`]: a run-time decision depends only on the
//!   profile and the command context, so a run can be replayed; a recipe
//!   compiles to one machine's identity for a host and seed.
//! - [`pattern`]: command patterns, shared by rules, resolvers and chaos.
//! - [`chaos`]: deterministic fault injection.
//! - [`network`]: a network's policy at its gateway (allowlist, header
//!   injection).
//! - [`intercept`]: the interception domains (exec, net, file): each one's
//!   context, decision, interceptor trait and chain.
//! - [`types`] and [`event`]: the values producers and consumers share, and
//!   the typed events a run records. `needy-trace` carries and stores
//!   them as JSON payloads.
//! - [`report`]: a run's events rendered as markdown.

pub mod chaos;
pub mod event;
pub mod intercept;
pub mod network;
pub mod pattern;
pub mod recipe;
pub mod report;
pub mod rules;
pub mod types;
