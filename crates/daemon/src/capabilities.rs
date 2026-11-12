//! What this machine can host as a runner, found at start: its
//! capabilities, each role with its runtimes or providers.
//!
//! - `actor(firecracker)`: Linux, a KVM device that opens for reading and
//!   writing (as Firecracker opens it), and a complete actor image (vmlinux,
//!   rootfs.ext4 and the patched firecracker).
//! - `actor(container)`: a Docker daemon that answers. Whether it has the
//!   OCI runtime a profile names (`runc`, gVisor's `runsc`) and the
//!   container actor image is checked when the run is created.
//! - `actor(macos-vz)`: macOS on Apple silicon, a `vfkit` that runs, and a
//!   complete `macos-vz` actor image (Image, rootfs.ext4).
//! - `actor(macos-apple-container)`: macOS, Apple's `container` CLI on `PATH`, and
//!   its services running (`container system status`). Whether its image
//!   store has the container actor image is checked when the run is
//!   created.
//! - `executor(docker)`: a Docker daemon that answers.
//! - `executor(native)`: always, on every platform a runner runs on.
//!
//! [`probe`] reads the machine; [`detect`] and [`restrict`] decide from what
//! it read, so the rules are tested without the machine.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use needy_core::types::{ActorRuntime, Capabilities, ExecutorProvider, RunnerRole};

// NOTICE: how long a runner waits for the Docker daemon at start. A local
// socket answers in milliseconds; a Docker Desktop that is not running
// fails at once. 3 s bounds a daemon that hangs without delaying the
// runner's join much.
const DOCKER_WAIT: Duration = Duration::from_secs(3);

/// What [`probe`] read from this machine: for each fact, the value or why
/// it is missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
  /// A Firecracker microVM can start: KVM and the actor image.
  pub firecracker: Result<(), String>,
  /// The runtimes the Docker daemon registered.
  pub docker: Result<BTreeSet<String>, String>,
  /// A Virtualization.framework VM can start: Apple silicon macOS, vfkit
  /// and the `macos-vz` actor image.
  pub macos_vz: Result<(), String>,
  /// Apple's `container` CLI is there and its services answer.
  pub macos_apple_container: Result<(), String>,
}

/// The capabilities found, and why each one that is not is missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
  pub capabilities: Capabilities,
  /// One line per missing runtime or provider, such as `actor(firecracker):
  /// cannot open /dev/kvm`.
  pub missing: Vec<String>,
}

/// The capabilities `facts` give, by the rules in the module documentation.
pub fn detect(facts: &Facts) -> Detected {
  let mut capabilities = Capabilities {
    executor: [ExecutorProvider::Native].into(),
    ..Default::default()
  };
  let mut missing = Vec::new();
  match &facts.firecracker {
    Ok(()) => {
      capabilities.actor.insert(ActorRuntime::Firecracker);
    }
    Err(why) => missing.push(format!("actor(firecracker): {why}")),
  }
  match &facts.docker {
    Ok(_runtimes) => {
      capabilities.executor.insert(ExecutorProvider::Docker);
      // TODO(runner-actor): the runtimes are not reported with the
      // capability; see `needy_core::types::ActorRuntime::Container`.
      capabilities.actor.insert(ActorRuntime::Container);
    }
    Err(why) => {
      missing.push(format!("executor(docker): {why}"));
      missing.push(format!("actor(container): {why}"));
    }
  }
  match &facts.macos_vz {
    Ok(()) => {
      capabilities.actor.insert(ActorRuntime::MacosVz);
    }
    Err(why) => missing.push(format!("actor(macos-vz): {why}")),
  }
  match &facts.macos_apple_container {
    Ok(()) => {
      capabilities.actor.insert(ActorRuntime::MacosAppleContainer);
    }
    Err(why) => missing.push(format!("actor(macos-apple-container): {why}")),
  }
  Detected {
    capabilities,
    missing,
  }
}

/// The capabilities a runner reports: those detected, within `roles` when
/// the operator named some (`--roles`), and a warning for each named role
/// this machine cannot host. Naming a role adds no capability.
pub fn restrict(detected: &Detected, roles: Option<&BTreeSet<RunnerRole>>) -> (Capabilities, Vec<String>) {
  let Some(roles) = roles else {
    return (detected.capabilities.clone(), Vec::new());
  };
  let held = detected.capabilities.roles();
  let warnings = roles
    .iter()
    .filter(|role| !held.contains(role))
    .map(|role| {
      let prefix = format!("{role}(");
      let why: Vec<&str> = detected.missing.iter().filter(|line| line.starts_with(&prefix)).map(String::as_str).collect();
      format!("--roles names {role}, which this machine cannot host ({}); it is left out", why.join("; "))
    })
    .collect();
  (detected.capabilities.within(roles), warnings)
}

/// Whether a Firecracker actor can start on this machine with the KVM
/// device `kvm` and the actor image in `image`.
pub fn firecracker(kvm: &Path, image: &Path) -> Result<(), String> {
  if !cfg!(target_os = "linux") {
    return Err("Firecracker runs on Linux only".into());
  }
  needy_core_actor::host::firecracker::check_kvm(kvm).map_err(|e| format!("cannot open {}: {}", kvm.display(), kvm_error(&e)))?;
  needy_core_actor::host::firecracker::Image::new(image).check().map_err(|e| e.to_string())
}

/// The OS error of a KVM check, without the check's advice.
fn kvm_error(error: &needy_core_actor::host::firecracker::Error) -> String {
  match error {
    needy_core_actor::host::firecracker::Error::Kvm { error, .. } => error.to_string(),
    other => other.to_string(),
  }
}

/// Whether a `macos-vz` actor can start on this machine with the actor image in
/// `image`: Apple silicon macOS, vfkit and the image.
pub async fn macos_vz(image: &Path) -> Result<(), String> {
  use needy_core_actor::host::macos::vz;
  if !vz::supported_host() {
    return Err("Virtualization.framework actors run on macOS on Apple silicon only".into());
  }
  vz::check_vfkit(Path::new(vz::VFKIT)).await.map_err(|e| e.to_string())?;
  vz::Image::new(image).check().map_err(|e| e.to_string())
}

/// Reads this machine: KVM at `kvm`, the actor image in `image`, the
/// Docker daemon, vfkit and Apple's `container`.
pub async fn probe(kvm: &Path, image: &Path) -> Facts {
  let docker = needy_core_executor::provider::docker::runtimes(DOCKER_WAIT)
    .await
    .map_err(|e| format!("no Docker daemon answers: {:#}", anyhow::Error::from(e)));
  Facts {
    firecracker: firecracker(kvm, image),
    docker,
    macos_vz: macos_vz(image).await,
    macos_apple_container: macos_apple_container().await,
  }
}

/// Whether Apple's `container` can run actors here: its CLI and services.
pub async fn macos_apple_container() -> Result<(), String> {
  #[cfg(unix)]
  {
    // The image does not matter for the services.
    let runtime = needy_core_actor::host::macos::apple_container::AppleContainer::new(needy_config::profile::CONTAINER_IMAGE.into());
    // NOTICE: `container system status` answers in about 50 ms on an Apple
    // silicon laptop; 3 s bounds services that hang, as `DOCKER_WAIT` does
    // for Docker.
    match tokio::time::timeout(DOCKER_WAIT, runtime.services()).await {
      Ok(services) => services.map_err(|e| e.to_string()),
      Err(_) => Err(format!("`container system status` did not answer within {DOCKER_WAIT:?}")),
    }
  }
  #[cfg(not(unix))]
  {
    Err("Apple's `container` runs on macOS only, on a Mac with Apple silicon".into())
  }
}

/// What this machine reports as a runner: probed, detected and restricted
/// to `roles` as [`restrict`] does, with what is missing logged. The
/// warnings are logged too, and returned for the operator.
pub async fn of_this_machine(kvm: &Path, image: &Path, roles: Option<&BTreeSet<RunnerRole>>) -> (Capabilities, Vec<String>) {
  let detected = detect(&probe(kvm, image).await);
  for line in &detected.missing {
    tracing::info!("not detected: {line}");
  }
  let (capabilities, warnings) = restrict(&detected, roles);
  for warning in &warnings {
    tracing::warn!("{warning}");
  }
  tracing::info!("capabilities: {capabilities}");
  (capabilities, warnings)
}

#[cfg(test)]
#[path = "capabilities_test.rs"]
mod tests;
