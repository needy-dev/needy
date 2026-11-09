use std::path::Path;

use needy_config::Source;
use needy_core::recipe::{CompiledRecipe, HostInfo, Smbios, VmIdentity};
use nix::sys::signal::Signal;
use serde_json::json;

use super::*;

const IMAGE: &str = "/home/user/.local/share/needy/actor";
const DIR: &str = "/home/user/.local/state/needy/sessions/0123456789ab";

fn gce(extra: &str) -> CompiledRecipe {
  let text = format!("extends: builtin:gce-ubuntu-22.04\n{extra}");
  let recipe = needy_config::recipe::load(&Source::text(&text, ".")).unwrap();
  needy_core::recipe::compile(&recipe, &HostInfo::default(), 7)
}

#[test]
fn a_vm_without_a_recipe_has_two_vcpus_and_no_pci_bus() {
  let launch = Launch::new(&Image::new(Path::new(IMAGE)), Path::new(DIR), None);

  assert_eq!((launch.vcpus, launch.mem_mib), (2, 2048));
  assert_eq!(
    launch.config,
    json!({
      "boot-source": {
        "kernel_image_path": "/home/user/.local/share/needy/actor/vmlinux",
        "boot_args": "console=ttyS0 reboot=k panic=1 root=/dev/vda ro init=/.needy/init quiet loglevel=4 pci=off",
      },
      "drives": [{
        "drive_id": "rootfs",
        "path_on_host": "/home/user/.local/share/needy/actor/rootfs.ext4",
        "is_root_device": true,
        "is_read_only": true,
      }],
      "machine-config": {"vcpu_count": 2, "mem_size_mib": 2048},
      "vsock": {"guest_cid": 3, "uds_path": "/home/user/.local/state/needy/sessions/0123456789ab/v.sock"},
    })
  );
  assert_eq!(
    launch.args,
    [
      "--no-api",
      "--config-file",
      "/home/user/.local/state/needy/sessions/0123456789ab/vm.json"
    ]
  );
  assert!(launch.env.is_empty(), "{:?}", launch.env);
}

#[test]
fn a_recipe_with_pci_bridges_sizes_the_vm_and_enables_pci() {
  let recipe = gce("");

  let launch = Launch::new(&Image::new(Path::new(IMAGE)), Path::new(DIR), Some(&recipe));

  assert_eq!((launch.vcpus, launch.mem_mib), (2, 4096));
  assert_eq!(
    launch.config["boot-source"]["boot_args"],
    "console=ttyS0 reboot=k panic=1 root=/dev/vda ro init=/.needy/init quiet loglevel=4"
  );
  assert_eq!(launch.config["machine-config"], json!({"vcpu_count": 2, "mem_size_mib": 4096}));
  assert_eq!(launch.config["cpu-config"], recipe.cpu_template);
  assert_eq!(
    launch.args,
    [
      "--no-api",
      "--config-file",
      "/home/user/.local/state/needy/sessions/0123456789ab/vm.json",
      "--enable-pci"
    ]
  );
}

#[test]
fn the_firecracker_environment_carries_the_recipe_identity() {
  let recipe = gce("cpu: { hypervisor: hidden }");

  let env = Launch::new(&Image::new(Path::new(IMAGE)), Path::new(DIR), Some(&recipe)).env;

  assert_eq!(env["NEEDY_BRAND_STRING"], "Intel(R) Xeon(R) CPU @ 2.20GHz");
  assert_eq!(env["NEEDY_HIDE_HYPERVISOR"], "1");
  assert_eq!(env["NEEDY_PCI_HOST_BRIDGE"], "8086:1237:02");
  assert_eq!(env["NEEDY_PCI_ISA_BRIDGE"], "8086:7110:03");
  let smbios = &env["NEEDY_SMBIOS"];
  assert!(smbios.starts_with("bios_date=01/01/2011;bios_vendor=Google;bios_version=Google;"), "{smbios}");
  assert!(smbios.contains(";product_name=Google Compute Engine;product_version=;"), "{smbios}");
  assert!(smbios.contains("chassis_type=1;"), "{smbios}");
  assert!(smbios.contains(";sys_vendor=Google;"), "{smbios}");
  assert!(smbios.ends_with(&format!(";uuid={}", recipe.identity.smbios.as_ref().unwrap().uuid)), "{smbios}");
}

#[test]
fn a_visible_hypervisor_sets_no_hide_variable() {
  let recipe = gce("");

  let env = Launch::new(&Image::new(Path::new(IMAGE)), Path::new(DIR), Some(&recipe)).env;

  assert!(!env.contains_key("NEEDY_HIDE_HYPERVISOR"), "{env:?}");
}

#[test]
fn smbios_values_cannot_end_a_field_early() {
  let recipe = CompiledRecipe {
    identity: VmIdentity {
      smbios: Some(Smbios {
        product_name: Some("a;b".into()),
        uuid: "u".into(),
        ..Default::default()
      }),
      ..Default::default()
    },
    ..gce("")
  };

  let env = Launch::new(&Image::new(Path::new(IMAGE)), Path::new(DIR), Some(&recipe)).env;

  assert_eq!(env["NEEDY_SMBIOS"], "product_name=a,b;uuid=u");
}

#[test]
fn a_missing_image_file_is_named_in_the_error() {
  let dir = tempfile::tempdir().unwrap();
  let launch = Launch::new(&Image::new(dir.path()), dir.path(), None);

  let Err(error) = Vm::start(&launch) else {
    panic!("a VM started without an image");
  };

  assert_eq!(error.to_string(), format!("actor image is missing {}; run `needy build`", dir.path().join("vmlinux").display()));
}

/// An image whose `firecracker` runs `script` with `sh`.
fn fake_image(dir: &Path, script: &str) -> Image {
  use std::os::unix::fs::PermissionsExt;
  let image = Image::new(dir);
  std::fs::write(&image.kernel, "").unwrap();
  std::fs::write(&image.rootfs, "").unwrap();
  std::fs::write(&image.firecracker, format!("#!/bin/sh\n{script}\n")).unwrap();
  std::fs::set_permissions(&image.firecracker, std::fs::Permissions::from_mode(0o755)).unwrap();
  image
}

#[tokio::test]
async fn stop_terminates_firecracker() {
  use std::os::unix::process::ExitStatusExt;
  let dir = tempfile::tempdir().unwrap();
  let image = fake_image(dir.path(), "exec sleep 300");
  let mut vm = Vm::start(&Launch::new(&image, dir.path(), None)).unwrap();

  let status = vm.stop(std::time::Duration::from_secs(10)).await.unwrap();

  assert_eq!(status.signal(), Some(Signal::SIGTERM as i32));
}

// A Firecracker stuck in a device's emulation may not handle the signal.
#[tokio::test]
async fn stop_kills_a_firecracker_that_ignores_sigterm() {
  use std::os::unix::process::ExitStatusExt;
  let dir = tempfile::tempdir().unwrap();
  let ready = dir.path().join("ready");
  let image = fake_image(dir.path(), &format!("trap '' TERM; touch {}; while :; do sleep 0.1; done", ready.display()));
  let mut vm = Vm::start(&Launch::new(&image, dir.path(), None)).unwrap();
  // The trap must be in place before the signal.
  while !ready.exists() {
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
  }

  let status = vm.stop(std::time::Duration::from_millis(200)).await.unwrap();

  assert_eq!(status.signal(), Some(Signal::SIGKILL as i32));
}
