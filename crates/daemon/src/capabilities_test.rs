use std::collections::BTreeSet;

use needy_core::types::RunnerRole;

use super::{Facts, detect, firecracker, restrict};

fn runtimes(names: &[&str]) -> Result<BTreeSet<String>, String> {
  Ok(names.iter().map(|n| n.to_string()).collect())
}

/// A Linux KVM host with Docker and the actor image.
fn kvm_host() -> Facts {
  Facts {
    firecracker: Ok(()),
    docker: runtimes(&["runc"]),
    vz: Err("Virtualization.framework actors run on macOS on Apple silicon only".into()),
    apple_container: Err("Apple's `container` runs on macOS only, on a Mac with Apple silicon".into()),
  }
}

/// A Mac without Docker running.
fn mac() -> Facts {
  Facts {
    firecracker: Err("Firecracker runs on Linux only".into()),
    docker: Err("no Docker daemon answers".into()),
    vz: Err("cannot run vfkit".into()),
    apple_container: Err("Apple's `container` CLI is not on PATH".into()),
  }
}

#[test]
fn a_kvm_host_with_docker_hosts_both_actor_runtimes_and_docker_and_native_executors() {
  let detected = detect(&kvm_host());

  assert_eq!(detected.capabilities.to_string(), "actor(container,firecracker) executor(docker,native)");
  assert_eq!(
    detected.missing,
    [
      "actor(vz): Virtualization.framework actors run on macOS on Apple silicon only",
      "actor(apple-container): Apple's `container` runs on macOS only, on a Mac with Apple silicon"
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
      "actor(firecracker): Firecracker runs on Linux only",
      "executor(docker): no Docker daemon answers",
      "actor(container): no Docker daemon answers",
      "actor(vz): cannot run vfkit",
      "actor(apple-container): Apple's `container` CLI is not on PATH",
    ]
  );
}

#[test]
fn a_mac_whose_apple_container_services_answer_hosts_apple_container_actors() {
  let facts = Facts {
    apple_container: Ok(()),
    ..mac()
  };

  let detected = detect(&facts);

  assert_eq!(detected.capabilities.to_string(), "actor(apple-container) executor(native)");
  assert!(!detected.missing.iter().any(|line| line.starts_with("actor(apple-container)")), "{:?}", detected.missing);
}

#[test]
fn a_docker_daemon_without_kvm_hosts_container_actors() {
  // `runc` is Docker's default runtime; which OCI runtime a profile names
  // is checked when its run is created.
  let facts = Facts {
    firecracker: Err("cannot open /dev/kvm: No such file or directory".into()),
    docker: runtimes(&["runc"]),
    vz: Err("Virtualization.framework actors run on macOS on Apple silicon only".into()),
    apple_container: Err("Apple's `container` runs on macOS only, on a Mac with Apple silicon".into()),
  };

  assert_eq!(detect(&facts).capabilities.to_string(), "actor(container) executor(docker,native)");
}

#[test]
fn roles_restrict_what_was_detected() {
  let roles: BTreeSet<RunnerRole> = [RunnerRole::Executor].into();

  let (capabilities, warnings) = restrict(&detect(&kvm_host()), Some(&roles));

  assert_eq!(capabilities.to_string(), "executor(docker,native)");
  assert!(warnings.is_empty(), "{warnings:?}");
  assert_eq!(restrict(&detect(&kvm_host()), None).0.to_string(), "actor(container,firecracker) executor(docker,native)");
}

#[test]
fn a_role_the_machine_cannot_host_is_warned_about_and_not_added() {
  let roles: BTreeSet<RunnerRole> = [RunnerRole::Actor, RunnerRole::Executor].into();

  let (capabilities, warnings) = restrict(&detect(&mac()), Some(&roles));

  assert_eq!(capabilities.to_string(), "executor(native)");
  assert_eq!(
    warnings,
    [
      "--roles names actor, which this machine cannot host (actor(firecracker): Firecracker runs on Linux only; \
      actor(container): no Docker daemon answers; actor(vz): cannot run vfkit; actor(apple-container): Apple's `container` CLI is \
      not on PATH); it is left out"
    ]
  );
}

#[test]
fn an_apple_silicon_mac_with_vfkit_and_the_image_hosts_vz_actors() {
  let facts = Facts {
    firecracker: Err("Firecracker runs on Linux only".into()),
    docker: Err("no Docker daemon answers".into()),
    vz: Ok(()),
    apple_container: Err("Apple's `container` CLI is not on PATH".into()),
  };

  assert_eq!(detect(&facts).capabilities.to_string(), "actor(vz) executor(native)");
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
#[tokio::test]
async fn vz_needs_apple_silicon_macos() {
  let dir = tempfile::tempdir().unwrap();

  assert_eq!(super::vz(dir.path()).await.unwrap_err(), "Virtualization.framework actors run on macOS on Apple silicon only");
}

#[cfg(target_os = "linux")]
#[test]
fn firecracker_needs_a_kvm_device_that_opens_and_a_complete_image() {
  let dir = tempfile::tempdir().unwrap();
  let kvm = dir.path().join("kvm");
  let image = dir.path().join("image");
  std::fs::create_dir(&image).unwrap();

  let no_kvm = firecracker(&kvm, &image).unwrap_err();
  std::fs::write(&kvm, b"").unwrap();
  let no_image = firecracker(&kvm, &image).unwrap_err();
  for file in ["vmlinux", "rootfs.ext4", "firecracker"] {
    std::fs::write(image.join(file), b"").unwrap();
  }

  assert!(no_kvm.starts_with(&format!("cannot open {}: ", kvm.display())), "{no_kvm}");
  assert_eq!(no_image, format!("actor image is missing {}; run `needy build`", image.join("vmlinux").display()));
  assert_eq!(firecracker(&kvm, &image), Ok(()));
}

#[cfg(not(target_os = "linux"))]
#[test]
fn firecracker_needs_linux() {
  let dir = tempfile::tempdir().unwrap();

  assert_eq!(firecracker(dir.path(), dir.path()).unwrap_err(), "Firecracker runs on Linux only");
}
