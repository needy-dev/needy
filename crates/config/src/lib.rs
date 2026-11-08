//! Profiles, recipes and routing rules.
//!
//! A [`profile::Profile`] describes one session: which machines to use, how
//! commands are routed and rewritten, file mappings and chaos. A
//! [`recipe::Recipe`] describes one machine and compiles into Firecracker and
//! guest settings. Run-time rules are declarative: a decision depends only on
//! the profile and the command context, so a session can be replayed.

pub mod profile;
pub mod recipe;
pub mod rules;
mod util;

pub use profile::{
    ActorConfig, Chaos, ExecutorConfig, Fault, FileMapping, NetworkPolicy, Profile, Runtime, User,
    check_docker_args, filter_allows,
};
pub use recipe::{CompiledRecipe, Finding, HostInfo, Recipe, Severity};
pub use rules::{CommandContext, Decision, Target};
