//! Needy's domain logic, shared by the daemon, actors, executors and the CLI.
//! Profiles and recipes are loaded by `needy-config`; this crate interprets
//! them.
//!
//! - [`rules`] and [`recipe`]: a run-time decision depends only on the
//!   profile and the command context, so a session can be replayed; a recipe
//!   compiles to one machine's settings for a host and seed.
//! - [`chaos`]: deterministic fault injection.
//! - [`trace`] and [`report`]: recording sessions and summarising them.

pub mod chaos;
pub mod docker;
pub mod recipe;
pub mod report;
pub mod rules;
pub mod store;
pub mod trace;
