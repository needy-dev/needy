use std::path::Path;

use needy_config::Source;
use needy_config::recipe::IdentityField;
use needy_core::recipe::{CompiledRecipe, HostInfo};

use super::*;
use crate::contract::GUEST_PORT;

const IMAGE: &str = "/Users/user/.local/share/needy/actor";
const DIR: &str = "/Users/user/.local/state/needy/runs/0123456789ab";

fn gce(extra: &str) -> CompiledRecipe {
  let text = format!("extends: builtin:gce-ubuntu-22.04\n{extra}");
  let recipe = needy_config::recipe::load(&Source::text(&text, ".")).unwrap();
  needy_core::recipe::compile(&recipe, &HostInfo::default(), &needy_core::recipe::PerRun::at(7, 0))
}

fn launch(recipe: Option<&CompiledRecipe>) -> Launch {
  Launch::new(&Image::new(Path::new(IMAGE)), Path::new(VFKIT), Path::new(DIR), recipe)
}

#[test]
fn a_vm_without_a_recipe_boots_the_image_read_only_with_vsock_bridges_and_no_network() {
  let launch = launch(None);

  assert_eq!((launch.vcpus, launch.mem_mib), (2, 2048));
  assert_eq!(launch.vfkit, Path::new("vfkit"));
  assert_eq!(
    launch.args,
    [
      "--cpus",
      "2",
      "--memory",
      "2048",
      "--bootloader",
      "linux,kernel=/Users/user/.local/share/needy/actor/Image,initrd=/Users/user/.local/share/needy/actor/initrd,\"cmdline=console=hvc0 root=/dev/vda ro init=/.needy/init quiet loglevel=4\"",
      "--device",
      "virtio-blk,path=/Users/user/.local/share/needy/actor/rootfs.ext4,readonly",
      "--device",
      "virtio-serial,logFilePath=/dev/stdout",
      "--device",
      "virtio-rng",
      "--device",
      "virtio-vsock,port=7000,socketURL=/Users/user/.local/state/needy/runs/0123456789ab/guest.sock",
      "--device",
      "virtio-vsock,port=7001,socketURL=/Users/user/.local/state/needy/runs/0123456789ab/gateway.sock",
      "--device",
      "virtio-vsock,port=7002,socketURL=/Users/user/.local/state/needy/runs/0123456789ab/process.sock,connect",
      "--pidfile",
      "/Users/user/.local/state/needy/runs/0123456789ab/vfkit.pid",
      "--restful-uri",
      "none://",
    ]
  );
  assert!(!launch.args.iter().any(|arg| arg.starts_with("virtio-net")), "{:?}", launch.args);
}

#[test]
fn a_recipe_sizes_the_vm() {
  let recipe = gce("");

  let launch = launch(Some(&recipe));

  assert_eq!((launch.vcpus, launch.mem_mib), (2, 8192));
  assert_eq!(launch.args[..4], ["--cpus", "2", "--memory", "8192"]);
}

#[test]
fn the_channels_are_the_vsock_bridges_and_the_ports_init_dials_under_firecracker() {
  let vz = MacosVz {
    image: Image::new(Path::new(IMAGE)),
    vfkit: VFKIT.into(),
  };
  let dir = Path::new(DIR);

  let channels = vz.channels(dir);

  assert_eq!(channels.guest, Endpoint::Unix(dir.join("guest.sock")));
  assert_eq!(channels.gateway, Endpoint::Unix(dir.join("gateway.sock")));
  assert_eq!(channels.terminal, Terminal::Unix(dir.join("process.sock")));
  // The guest's init is the Firecracker actor's: it dials the daemon on
  // vsock port 7000 and is told the gateway and process ports.
  assert_eq!((GUEST_PORT, channels.gateway_port, channels.process_port), (7000, 7001, 7002));
  assert_eq!(
    channels.sockets,
    [
      dir.join("guest.sock"),
      dir.join("gateway.sock"),
      dir.join("process.sock")
    ]
  );
  assert_eq!(FACETS.transport, Transport::Vsock);
  assert_eq!((FACETS.workspace, FACETS.identity.hwmon), (Side::Guest, Some(Side::Guest)));
}

#[test]
fn an_image_without_its_kernel_is_missing_the_kernel() {
  let dir = tempfile::tempdir().unwrap();
  std::fs::write(dir.path().join("rootfs.ext4"), "").unwrap();

  let error = Image::new(dir.path()).check().unwrap_err();

  assert!(matches!(&error, Error::Missing(path) if path == &dir.path().join("Image")), "{error:?}");
}

#[test]
fn a_plain_machine_has_no_identity_gaps() {
  assert_eq!(gaps(None), []);
}

#[test]
fn an_x86_recipe_is_not_faithful_on_arm64() {
  let recipe = gce("");

  let gaps = gaps(Some(&recipe));

  let fields: Vec<_> = gaps.iter().map(|gap| gap.field).collect();
  assert_eq!(
    fields,
    [
      None,
      Some(IdentityField::Cpuid),
      Some(IdentityField::Cpuinfo),
      Some(IdentityField::Dmi),
      Some(IdentityField::Pci)
    ]
  );
  assert!(gaps[0].message.contains("x86_64") && gaps[0].message.contains("arm64"), "{}", gaps[0].message);
  // The guest kernel, init and fake sensors are the Firecracker actor's.
  for field in [
    IdentityField::Hostname,
    IdentityField::Hwmon,
    IdentityField::Kernel,
    IdentityField::Uptime,
  ] {
    assert!(!fields.contains(&Some(field)), "{field:?} is a gap: {gaps:?}");
  }
}

#[tokio::test]
async fn a_missing_vfkit_fails_its_check_naming_how_to_install_it() {
  let error = check_vfkit(Path::new("/nonexistent/vfkit")).await.unwrap_err().to_string();

  assert!(error.contains("cannot run /nonexistent/vfkit") && error.contains("brew install vfkit"), "{error}");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[tokio::test]
async fn an_apple_silicon_mac_without_vfkit_fails_the_host_check() {
  let vz = MacosVz {
    image: Image::new(Path::new(IMAGE)),
    vfkit: "/nonexistent/vfkit".into(),
  };

  let error = vz.check_host().await.unwrap_err().to_string();

  assert!(error.contains("brew install vfkit"), "{error}");
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
#[tokio::test]
async fn another_host_fails_the_check_naming_the_runtimes_it_has() {
  let vz = MacosVz {
    image: Image::new(Path::new(IMAGE)),
    vfkit: VFKIT.into(),
  };

  let error = vz.check_host().await.unwrap_err().to_string();

  assert!(error.contains("macOS on Apple silicon only") && error.contains("driver: linux-firecracker"), "{error}");
}

#[tokio::test]
async fn stop_ends_a_vm_that_ignores_sigterm_after_the_grace() {
  let dir = tempfile::tempdir().unwrap();
  for file in ["Image", "initrd", "rootfs.ext4"] {
    std::fs::write(dir.path().join(file), "").unwrap();
  }
  // A stand-in vfkit that ignores SIGTERM and logs to its output.
  let vfkit = dir.path().join("vfkit");
  std::fs::write(&vfkit, "#!/bin/sh\ntrap '' TERM\necho vfkit started\nwhile :; do sleep 1; done\n").unwrap();
  std::fs::set_permissions(&vfkit, <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755)).unwrap();
  let launch = Launch::new(&Image::new(dir.path()), &vfkit, dir.path(), None);
  let mut vm = Vm::start(&launch).unwrap();
  let log = crate::driver::log(dir.path());
  for _ in 0..100 {
    if std::fs::read_to_string(&log).unwrap_or_default().contains("vfkit started") {
      break;
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
  }

  let status = vm.stop(Duration::from_millis(200)).await.unwrap();

  use std::os::unix::process::ExitStatusExt;
  assert_eq!(status.signal(), Some(nix::sys::signal::Signal::SIGKILL as i32));
  assert!(std::fs::read_to_string(&log).unwrap().contains("vfkit started"));
}

// Moved from the daemon's capabilities tests with detection.
#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
#[tokio::test]
async fn detection_needs_apple_silicon_macos() {
  let dir = tempfile::tempdir().unwrap();
  let host = Host {
    kvm: dir.path().to_path_buf(),
    image: dir.path().to_path_buf(),
  };

  assert_eq!(detect(&host).await.unwrap_err(), "Virtualization.framework actors run on macOS on Apple silicon only");
}
