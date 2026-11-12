//! The Firecracker configuration of one VM from a compiled recipe: `vm.json`,
//! Firecracker's arguments, and the environment the patched Firecracker reads
//! to present the recipe's identity.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use needy_core::recipe::{CompiledRecipe, VmIdentity};
use serde_json::json;

use super::{Image, vsock};

/// The guest's vsock context ID; the host is always 2.
// Source: https://github.com/torvalds/linux/blob/v6.18/include/uapi/linux/vm_sockets.h#L126-L131 (`VMADDR_CID_HOST`
// is 2); 3 is the first CID left for guests and Firecracker's own example,
// https://github.com/firecracker-microvm/firecracker/blob/95f868c8e345b1cc8faccd1a3c910b4989dc3f58/docs/vsock.md#L114-L126 (with `v.sock_<port>` for guest-initiated
// connections, which `host_port` builds).
const GUEST_CID: u32 = 3;

/// The guest kernel's command line. `init=` starts needy's guest init.
// Source: https://github.com/firecracker-microvm/firecracker/blob/95f868c8e345b1cc8faccd1a3c910b4989dc3f58/docs/getting-started.md#L260
// (`console=ttyS0 reboot=k panic=1`); `reboot=k` makes a guest reboot end the
// Firecracker process on x86: https://github.com/firecracker-microvm/firecracker/blob/95f868c8e345b1cc8faccd1a3c910b4989dc3f58/FAQ.md#L181-L204.
// `pci=off` (added below for a VM without PCI identity) was in older
// Firecracker defaults; v1.17.0's default command line no longer has it,
// https://github.com/firecracker-microvm/firecracker/blob/95f868c8e345b1cc8faccd1a3c910b4989dc3f58/src/vmm/src/vmm_config/boot_source.rs#L9-L20.
// Reference: https://docs.kernel.org/6.18/admin-guide/kernel-parameters.html
const BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 root=/dev/vda ro init=/.needy/init quiet loglevel=4";

/// How one VM starts: Firecracker's configuration, arguments and
/// environment.
#[derive(Debug, Clone, PartialEq)]
pub struct Launch {
  pub image: Image,
  /// The VM's directory.
  pub dir: PathBuf,
  pub vcpus: u32,
  pub mem_mib: u32,
  /// The contents of `vm.json`.
  pub config: serde_json::Value,
  pub args: Vec<String>,
  /// What the patched Firecracker reads to present the recipe's identity.
  pub env: BTreeMap<String, String>,
}

impl Launch {
  /// The VM for `recipe`, or a plain 2 vCPU, 2 GiB VM without one.
  pub fn new(image: &Image, dir: &Path, recipe: Option<&CompiledRecipe>) -> Self {
    let (vcpus, mem_mib) = crate::driver::size(recipe);
    let pci = recipe.is_some_and(|r| r.identity.has_pci());
    let mut boot_args = BOOT_ARGS.to_string();
    if !pci {
      boot_args.push_str(" pci=off");
    }
    // NOTICE: JSON strings are Unicode, so a path that is not UTF-8 cannot
    // reach Firecracker's configuration at all; it is passed lossily and
    // Firecracker reports the file it cannot open.
    let path = |p: &Path| p.to_string_lossy().into_owned();
    let mut config = json!({
      "boot-source": {"kernel_image_path": path(&image.kernel), "boot_args": boot_args},
      "drives": [{"drive_id": "rootfs", "path_on_host": path(&image.rootfs), "is_root_device": true, "is_read_only": true}],
      "machine-config": {"vcpu_count": vcpus, "mem_size_mib": mem_mib},
      "vsock": {"guest_cid": GUEST_CID, "uds_path": path(&vsock(dir))},
    });
    // TODO(actor-network): the recipe's `network_mac` (and `interface`) are
    // unused in actors: the VM gets no `network-interfaces` entry, so the
    // guest has only `lo` and reaches the network through the gateway
    // forwarder over vsock. Giving the actor a NIC that carries the recipe's
    // MAC waits for an owner decision on the actor's network.
    if let Some(r) = recipe
      && r.cpu_template.get("cpuid_modifiers").and_then(|m| m.as_array()).is_some_and(|m| !m.is_empty())
    {
      config["cpu-config"] = r.cpu_template.clone();
    }
    let mut args = vec![
      "--no-api".to_string(),
      "--config-file".to_string(),
      path(&dir.join("vm.json")),
    ];
    if pci {
      args.push("--enable-pci".into());
    }
    Self {
      image: image.clone(),
      dir: dir.to_path_buf(),
      vcpus,
      mem_mib,
      config,
      args,
      env: recipe.map(|r| identity_env(&r.identity)).unwrap_or_default(),
    }
  }
}

/// The patched Firecracker's environment for a VM identity; see
/// `images/firecracker/needy.patch`.
fn identity_env(identity: &VmIdentity) -> BTreeMap<String, String> {
  let mut env = BTreeMap::new();
  if let Some(brand) = &identity.cpu_brand {
    env.insert("NEEDY_BRAND_STRING".into(), brand.clone());
  }
  if identity.hide_hypervisor {
    env.insert("NEEDY_HIDE_HYPERVISOR".into(), "1".into());
  }
  if let Some(smbios) = &identity.smbios {
    // `key=value;…`; a `;` inside a value would end it early. The keys are
    // the names the patch's `DEFAULTS` table reads, which are Linux's
    // `/sys/class/dmi/id` attribute names,
    // https://github.com/torvalds/linux/blob/v6.18/drivers/firmware/dmi-id.c#L41-L59
    // Every key whose patch default is a GCE value is sent, empty when the
    // recipe has none, so no default leaks into another machine's identity.
    let chassis_type = smbios.chassis_type.map(|n| n.to_string());
    let fields = [
      ("bios_date", smbios.bios_date.as_deref()),
      ("bios_vendor", smbios.bios_vendor.as_deref()),
      ("bios_version", smbios.bios_version.as_deref()),
      ("board_asset_tag", smbios.board_asset_tag.as_deref()),
      ("board_name", smbios.board_name.as_deref()),
      ("board_serial", smbios.board_serial.as_deref()),
      ("board_vendor", smbios.board_vendor.as_deref()),
      ("chassis_type", chassis_type.as_deref()),
      ("chassis_vendor", smbios.chassis_vendor.as_deref()),
      ("product_name", smbios.product_name.as_deref()),
      ("product_serial", smbios.serial.as_deref()),
      ("product_uuid", Some(smbios.uuid.as_str())),
      ("product_version", smbios.product_version.as_deref()),
      ("sys_vendor", smbios.sys_vendor.as_deref()),
    ];
    let value =
      fields.iter().map(|(key, value)| format!("{key}={}", value.unwrap_or_default().replace(';', ","))).collect::<Vec<_>>().join(";");
    env.insert("NEEDY_SMBIOS".into(), value);
  }
  if let Some(id) = identity.pci_host_bridge {
    env.insert("NEEDY_PCI_HOST_BRIDGE".into(), id.to_string());
  }
  if let Some(id) = identity.pci_isa_bridge {
    env.insert("NEEDY_PCI_ISA_BRIDGE".into(), id.to_string());
  }
  env
}
