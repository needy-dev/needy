use std::collections::BTreeSet;

use needy_config::profile::{ActorDriver, ExecutorDriver};
use needy_core::types::RunnerRole;

use super::{Facts, detect, restrict};

/// Facts with each actor driver's and executor driver's answer, in the
/// order of `ActorDriver::ALL` and `ExecutorDriver::ALL`.
fn facts(actor: [Result<(), &str>; 4], executor: [Result<(), &str>; 2]) -> Facts {
  let answer = |found: Result<(), &str>| found.map_err(str::to_string);
  Facts {
    actor: ActorDriver::ALL.into_iter().zip(actor.map(answer)).collect(),
    executor: ExecutorDriver::ALL.into_iter().zip(executor.map(answer)).collect(),
  }
}

/// A Linux KVM host with Docker and the actor image.
fn kvm_host() -> Facts {
  facts(
    [
      Ok(()),
      Ok(()),
      Err("Virtualization.framework actors run on macOS on Apple silicon only"),
      Err("Apple's `container` runs on macOS only, on a Mac with Apple silicon"),
    ],
    [Ok(()), Ok(())],
  )
}

/// A Mac without Docker running.
fn mac() -> Facts {
  facts(
    [
      Err("Firecracker runs on Linux only"),
      Err("no Docker-compatible engine answers"),
      Err("cannot run vfkit"),
      Err("Apple's `container` CLI is not on PATH"),
    ],
    [Ok(()), Err("no Docker-compatible engine answers")],
  )
}

#[test]
fn a_kvm_host_with_docker_hosts_both_actor_drivers_and_both_executor_drivers() {
  let detected = detect(&kvm_host());

  assert_eq!(detected.capabilities.to_string(), "actor(linux-container,linux-firecracker) executor(linux-container,native)");
  assert_eq!(
    detected.missing,
    [
      "actor(macos-vz): Virtualization.framework actors run on macOS on Apple silicon only",
      "actor(macos-apple-container): Apple's `container` runs on macOS only, on a Mac with Apple silicon"
    ]
  );
}

#[test]
fn a_machine_without_kvm_or_docker_hosts_native_executors_only() {
  let detected = detect(&mac());

  assert_eq!(detected.capabilities.to_string(), "executor(native)");
  assert_eq!(
    detected.missing,
    [
      "actor(linux-firecracker): Firecracker runs on Linux only",
      "actor(linux-container): no Docker-compatible engine answers",
      "actor(macos-vz): cannot run vfkit",
      "actor(macos-apple-container): Apple's `container` CLI is not on PATH",
      "executor(linux-container): no Docker-compatible engine answers",
    ]
  );
}

#[test]
fn a_mac_whose_apple_container_services_answer_hosts_macos_apple_container_actors() {
  let mut facts = mac();
  facts.actor.insert(ActorDriver::MacosAppleContainer, Ok(()));

  let detected = detect(&facts);

  assert_eq!(detected.capabilities.to_string(), "actor(macos-apple-container) executor(native)");
  assert!(!detected.missing.iter().any(|line| line.starts_with("actor(macos-apple-container)")), "{:?}", detected.missing);
}

#[test]
fn a_docker_daemon_without_kvm_hosts_container_actors() {
  // `runc` is Docker's default runtime; which OCI runtime a profile names
  // is checked when its run is created.
  let mut facts = kvm_host();
  facts.actor.insert(ActorDriver::LinuxFirecracker, Err("cannot open /dev/kvm: No such file or directory".into()));

  assert_eq!(detect(&facts).capabilities.to_string(), "actor(linux-container) executor(linux-container,native)");
}

#[test]
fn roles_restrict_what_was_detected() {
  let roles: BTreeSet<RunnerRole> = [RunnerRole::Executor].into();

  let (capabilities, warnings) = restrict(&detect(&kvm_host()), Some(&roles));

  assert_eq!(capabilities.to_string(), "executor(linux-container,native)");
  assert!(warnings.is_empty(), "{warnings:?}");
  assert_eq!(
    restrict(&detect(&kvm_host()), None).0.to_string(),
    "actor(linux-container,linux-firecracker) executor(linux-container,native)"
  );
}

#[test]
fn a_role_the_machine_cannot_host_is_warned_about_and_not_added() {
  let roles: BTreeSet<RunnerRole> = [RunnerRole::Actor, RunnerRole::Executor].into();

  let (capabilities, warnings) = restrict(&detect(&mac()), Some(&roles));

  assert_eq!(capabilities.to_string(), "executor(native)");
  assert_eq!(
    warnings,
    [
      "--roles names actor, which this machine cannot host (actor(linux-firecracker): Firecracker runs on Linux only; \
      actor(linux-container): no Docker-compatible engine answers; actor(macos-vz): cannot run vfkit; actor(macos-apple-container): \
      Apple's `container` CLI is not on PATH); it is left out"
    ]
  );
}

#[test]
fn an_apple_silicon_mac_with_vfkit_and_the_image_hosts_macos_vz_actors() {
  let mut facts = mac();
  facts.actor.insert(ActorDriver::MacosVz, Ok(()));

  assert_eq!(detect(&facts).capabilities.to_string(), "actor(macos-vz) executor(native)");
}
