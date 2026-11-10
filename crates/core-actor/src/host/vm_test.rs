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
  assert!(smbios.contains(";product_name=Google Compute Engine;"), "{smbios}");
  assert!(smbios.contains(";product_version=;"), "{smbios}");
  assert!(smbios.contains("chassis_type=1;"), "{smbios}");
  assert!(smbios.ends_with(";sys_vendor=Google"), "{smbios}");
  assert!(smbios.contains(&format!(";product_uuid={};", recipe.identity.smbios.as_ref().unwrap().uuid)), "{smbios}");
}

/// The keys `images/firecracker/needy.patch` reads from `NEEDY_SMBIOS`, with
/// their defaults: the `("key", "value")` rows of its `DEFAULTS` table.
fn patch_smbios_defaults() -> BTreeMap<String, String> {
  let patch = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../images/firecracker/needy.patch");
  let text = std::fs::read_to_string(&patch).unwrap();
  let table = text.split("const DEFAULTS").nth(1).unwrap().split("];").next().unwrap();
  table
    .lines()
    .filter_map(|line| {
      let row = line.trim_start_matches('+').trim().strip_prefix("(\"")?.strip_suffix("\"),")?;
      let (key, value) = row.split_once("\", \"")?;
      Some((key.to_string(), value.to_string()))
    })
    .collect()
}

fn smbios_keys(value: &str) -> BTreeMap<String, String> {
  value.split(';').filter_map(|pair| pair.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

// ROOT CAUSE:
//
// If a recipe set a serial, the guest still showed the patch's fixed GCE
// serial and UUID, because `identity_env` sent them as `serial` and `uuid`
// while the patched Firecracker reads `product_serial` and `product_uuid`
// (the names of `/sys/class/dmi/id`), and it never sent `chassis_vendor` or
// `board_serial`, so those stayed Google's on every machine.
//
// Before the fix, `dmidecode -t system` in an actor showed
// `GoogleCloud-6B1F5C2E0D4A9E8B7C3D2A1F0E9D8C7B` whatever the recipe said.
// The fix keeps every key needy sends one the patch reads, and sends every
// key whose patch default is a GCE value, so no default leaks into the guest.
#[test]
fn every_smbios_key_needy_sends_is_one_the_patched_firecracker_reads() {
  let recipe = gce("firmware: { dmi: { serial: \"GoogleCloud-{hash}\" } }");
  let smbios = recipe.identity.smbios.clone().unwrap();
  let defaults = patch_smbios_defaults();
  assert!(defaults.contains_key("product_serial"), "{defaults:?}");

  let env = Launch::new(&Image::new(Path::new(IMAGE)), Path::new(DIR), Some(&recipe)).env;
  let sent = smbios_keys(&env["NEEDY_SMBIOS"]);

  let unread: Vec<_> = sent.keys().filter(|key| !defaults.contains_key(*key)).collect();
  assert!(unread.is_empty(), "the patch does not read {unread:?}");
  let leaked: Vec<_> = defaults.iter().filter(|(key, value)| !value.is_empty() && !sent.contains_key(*key)).map(|(key, _)| key).collect();
  assert!(leaked.is_empty(), "the patch's defaults stay for {leaked:?}");
  assert_eq!(sent["product_serial"], smbios.serial.unwrap());
  assert_eq!(sent["product_uuid"], smbios.uuid);
  assert_eq!(sent["chassis_vendor"], "Google");
}

#[test]
fn a_dell_identity_sends_no_google_value() {
  let recipe = needy_config::recipe::load(&Source::builtin("dell-optiplex-ubuntu-24.04")).unwrap();
  let recipe = needy_core::recipe::compile(&recipe, &HostInfo::default(), 7);

  let env = Launch::new(&Image::new(Path::new(IMAGE)), Path::new(DIR), Some(&recipe)).env;
  let sent = smbios_keys(&env["NEEDY_SMBIOS"]);

  assert!(sent.values().all(|v| !v.contains("Google")), "{sent:?}");
  assert_eq!(sent["chassis_vendor"], "Dell Inc.");
  assert_eq!(sent["board_serial"], "");
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

  assert_eq!(
    env["NEEDY_SMBIOS"],
    "bios_date=;bios_vendor=;bios_version=;board_name=;board_serial=;board_vendor=;chassis_type=;chassis_vendor=;product_name=a,b;product_serial=;product_uuid=u;product_version=;sys_vendor="
  );
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
