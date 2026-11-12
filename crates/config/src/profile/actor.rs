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
  /// How a `container` or `macos-apple-container` actor runs; required with
  /// `container`, optional with `macos-apple-container`, refused with
  /// `firecracker`.
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
  /// A Virtualization.framework VM through vfkit, on Apple silicon macOS:
  /// needy's arm64 kernel and rootfs, as Firecracker boots them on Linux.
  #[serde(rename = "macos-vz")]
  MacosVz,
  /// A container under Apple's `container` on a Mac with Apple silicon,
  /// which runs each container in its own lightweight VM.
  #[serde(rename = "macos-apple-container")]
  MacosAppleContainer,
}

/// A container actor's options. `validate::profile` checks which apply to
/// the actor's runtime.
// NOTICE: `oci_runtime` has no default, for the reason `runtime` has none:
// `runc` and `runsc` isolate differently, and needy never chooses that for
// the profile. It is optional here only because `macos-apple-container` refuses
// it; `container` requires it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerActor {
  /// The OCI runtime Docker runs the container with: `container` only.
  #[serde(default)]
  pub oci_runtime: Option<OciRuntime>,
  /// The container actor image: `needy build --container` builds it for
  /// Docker, `needy build --macos-apple-container` for Apple's `container`.
  #[serde(default = "default_container_image")]
  pub image: String,
}

impl ContainerActor {
  /// The image of an actor whose profile has no `container:` options.
  pub fn image_of(options: Option<&ContainerActor>) -> String {
    options.map_or_else(default_container_image, |o| o.image.clone())
  }
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
      Runtime::MacosVz => "macos-vz",
      Runtime::MacosAppleContainer => "macos-apple-container",
    }
  }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
  pub single_process: bool,
  pub self_replace: bool,
}
