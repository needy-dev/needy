//! The actor: the machine where the driving program runs.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::network::NetworkPolicy;
use super::runner::RunnerSelector;
use crate::value::Words;

// NOTICE: `runtime` is required and has no default, so `Actor` has no
// container-level `#[serde(default)]`: needy never chooses an actor's
// isolation for the profile (the explicit-runtime decision in
// `docs/ai/references/architecture/2026-11-10-container-actor-runtime-design.md`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Actor {
  /// What runs the actor.
  pub runtime: Runtime,
  /// The runner the actor runs on; the control host itself when absent.
  #[serde(default)]
  pub runner: Option<RunnerSelector>,
  /// A recipe reference: `builtin:NAME` or a path relative to the profile.
  #[serde(default)]
  pub recipe: Option<String>,
  #[serde(default)]
  pub limits: Limits,
  #[serde(default)]
  pub network: NetworkPolicy,
  /// The driving program.
  #[serde(default)]
  pub command: Words,
  #[serde(default)]
  pub env: BTreeMap<String, String>,
  #[serde(default)]
  pub cwd: Option<String>,
}

/// What runs an actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
  /// A Firecracker microVM; needs `/dev/kvm`.
  Firecracker,
  /// An OCI container through Docker, for hosts without KVM.
  // TODO(container-runtime): the schema accepts it so that a profile can name
  // it, and `validate::profile` refuses it until slice 2 of the container
  // actor runtime design lands, with its `actor.container` options.
  Container,
}

impl Runtime {
  /// The name a profile gives it.
  pub fn as_str(self) -> &'static str {
    match self {
      Runtime::Firecracker => "firecracker",
      Runtime::Container => "container",
    }
  }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
  pub single_process: bool,
  pub self_replace: bool,
}
