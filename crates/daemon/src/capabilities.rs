//! What this machine can host as a runner, found at start: its
//! capabilities, each role with its drivers.
//!
//! Each driver says whether this machine has it, in its own module
//! (`needy_core_actor::driver::detect`, `needy_core_executor::driver::detect`):
//!
//! - `actor(linux-firecracker)`: Linux, a KVM device that opens for reading
//!   and writing (as Firecracker opens it), and a complete actor image
//!   (vmlinux, rootfs.ext4 and the patched firecracker).
//! - `actor(linux-container)`: a Docker-compatible engine that answers.
//!   Whether it has the OCI runtime a profile names (`runc`, gVisor's
//!   `runsc`) and the container actor image is checked when the run is
//!   created.
//! - `actor(macos-vz)`: macOS on Apple silicon, a `vfkit` that runs, and a
//!   complete `macos-vz` actor image (Image, rootfs.ext4).
//! - `actor(macos-apple-container)`: macOS, Apple's `container` CLI on
//!   `PATH`, and its services running (`container system status`). Whether
//!   its image store has the container actor image is checked when the run
//!   is created.
//! - `executor(linux-container)`: a Docker-compatible engine that answers.
//! - `executor(native)`: always, on every platform a runner runs on.
//!
//! [`probe`] reads the machine; [`detect`] and [`restrict`] decide from what
//! it read, so the rules are tested without the machine.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use needy_config::profile::{ActorDriver, ExecutorDriver};
use needy_core::types::{Capabilities, RunnerRole};

/// What [`probe`] read from this machine: for each driver, whether it has
/// it or why not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
  pub actor: BTreeMap<ActorDriver, Result<(), String>>,
  pub executor: BTreeMap<ExecutorDriver, Result<(), String>>,
}

/// The capabilities found, and why each one that is not is missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
  pub capabilities: Capabilities,
  /// One line per missing driver, such as `actor(linux-firecracker):
  /// cannot open /dev/kvm`.
  pub missing: Vec<String>,
}

/// The capabilities `facts` give: each driver this machine has.
pub fn detect(facts: &Facts) -> Detected {
  let mut capabilities = Capabilities::default();
  let mut missing = Vec::new();
  for (driver, found) in &facts.actor {
    match found {
      Ok(()) => {
        capabilities.actor.insert(*driver);
      }
      Err(why) => missing.push(format!("{}({driver}): {why}", RunnerRole::Actor)),
    }
  }
  for (driver, found) in &facts.executor {
    match found {
      Ok(()) => {
        capabilities.executor.insert(*driver);
      }
      Err(why) => missing.push(format!("{}({driver}): {why}", RunnerRole::Executor)),
    }
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

/// Reads this machine: asks every actor driver, with the KVM device `kvm`
/// and the actor image in `image`, and every executor driver whether this
/// machine has it.
pub async fn probe(kvm: &Path, image: &Path) -> Facts {
  let host = needy_core_actor::driver::Host {
    image: image.to_path_buf(),
    kvm: kvm.to_path_buf(),
    // Detection starts no actor, so it makes no guest disk.
    guest_disks: std::path::PathBuf::new(),
  };
  let mut facts = Facts {
    actor: BTreeMap::new(),
    executor: BTreeMap::new(),
  };
  // Both `linux-container` drivers ask the one engine, once.
  let engine = needy_core_executor::engine::Probe::default();
  for driver in ActorDriver::ALL {
    facts.actor.insert(driver, needy_core_actor::driver::detect(driver, &host, &engine).await);
  }
  for driver in ExecutorDriver::ALL {
    facts.executor.insert(driver, needy_core_executor::driver::detect(driver, &engine).await);
  }
  facts
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
