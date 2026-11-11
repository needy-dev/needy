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
  /// How a `container` actor runs; required with that runtime, refused
  /// with any other.
  #[serde(default)]
  pub container: Option<ContainerActor>,
}

/// What runs an actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
  /// A Firecracker microVM; needs `/dev/kvm`.
  Firecracker,
  /// An OCI container through Docker, for hosts without KVM.
  Container,
}

/// A container actor's options.
// NOTICE: `oci_runtime` has no default, for the reason `runtime` has none:
// `runc` and `runsc` isolate differently, and needy never chooses that for
// the profile.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerActor {
  /// The OCI runtime Docker runs the container with.
  pub oci_runtime: OciRuntime,
  /// The container actor image `needy build --container` builds.
  #[serde(default = "default_container_image")]
  pub image: String,
}

/// The image of a container actor whose profile names none.
pub const CONTAINER_IMAGE: &str = "needy-actor:latest";

fn default_container_image() -> String {
  CONTAINER_IMAGE.into()
}

/// The OCI runtime of a container actor, by the name Docker registers it
/// under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OciRuntime {
  /// Namespaces and cgroups on the host's kernel.
  Runc,
  /// gVisor: a user-space kernel between the program and the host.
  Runsc,
}

impl OciRuntime {
  /// The name Docker registers the runtime under (`docker info`'s
  /// `Runtimes`).
  pub fn as_str(self) -> &'static str {
    match self {
      OciRuntime::Runc => "runc",
      OciRuntime::Runsc => "runsc",
    }
  }
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
