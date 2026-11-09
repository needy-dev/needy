//! Needy's domain logic, shared by the daemon, actors, executors and the CLI.
//! Profiles and recipes are loaded by `needy-config`; this crate interprets
//! them.
//!
//! - [`rules`] and [`recipe`]: a run-time decision depends only on the
//!   profile and the command context, so a session can be replayed; a recipe
//!   compiles to one machine's identity for a host and seed.
//! - [`pattern`]: command patterns, shared by rules, resolvers and chaos.
//! - [`chaos`]: deterministic fault injection.
//! - [`intercept`]: the interceptor traits of each hook point.
//! - [`types`] and [`event`]: the values producers and consumers share, and
//!   the typed events a session records. `needy-trace` carries and stores
//!   them as JSON payloads.
//! - [`report`]: a session's events rendered as markdown.

pub mod chaos;
pub mod docker;
pub mod event;
pub mod intercept;
pub mod pattern;
pub mod recipe;
pub mod report;
pub mod rules;
pub mod types;
