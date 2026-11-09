//! Needy's domain logic, shared by the daemon, actors, executors and the CLI.
//!
//! - [`profile`], [`rules`] and [`recipe`]: declarative configuration. A run-time
//!   decision depends only on the profile and the command context, so a
//!   session can be replayed.
//! - [`chaos`]: deterministic fault injection.
//! - [`trace`] and [`report`]: recording sessions and summarising them.

pub mod chaos;
pub mod profile;
pub mod recipe;
pub mod report;
pub mod rules;
pub mod trace;

pub use profile::{
  ActorConfig, Chaos, ExecutorConfig, Fault, FileMapping, NetworkPolicy, Profile, Runtime, User, check_docker_args, filter_allows,
};
pub use recipe::{CompiledRecipe, Finding, HostInfo, Recipe, Severity};
pub use rules::{CommandContext, Decision, Target};
