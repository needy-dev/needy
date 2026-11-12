//! The `linux-firecracker` actor driver: the actor's Firecracker VM, its
//! configuration from a compiled recipe, and a handle that owns the
//! Firecracker process.
//!
//! A VM keeps its files in one directory: `vm.json` (the configuration),
//! `console.log` (the guest's serial console) and `v.sock` (the vsock device,
//! where guest-initiated connections to host port `P` land on `v.sock_P`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use needy_core::recipe::{CompiledRecipe, VmIdentity};
use serde_json::json;

pub use super::driver::Error;
use super::driver::{Channels, Driver, Endpoint, Running, Setup, Start, Terminal};

/// Ports the actor reaches on the host over vsock.
// NOTICE: needy's own choices; the guest's init dials `GUEST_PORT` as
// `vsock://2:7000` in `crates/core-actor/src/guest/mod.rs`.
pub const GUEST_PORT: u32 = 7000;
pub const GATEWAY_PORT: u32 = 7001;
/// The port the guest serves `ProcessService` on for a driving program on
/// a terminal; the host connects to it.
pub const PROCESS_PORT: u32 = 7002;

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

/// The KVM device Firecracker opens.
// Source: https://github.com/rust-vmm/kvm/blob/b4c9ed8df95a9e10a68f50f5ef5e7d04108759ba/kvm-ioctls/src/ioctls/system.rs#L97-L128
// (kvm-ioctls v0.25.0, which Firecracker v1.17.0 uses: `Kvm::new` opens
// `/dev/kvm` with `O_RDWR`).
// Reference: https://www.kernel.org/doc/html/v6.12/virt/kvm/api.html#general-description
pub const KVM: &str = "/dev/kvm";

/// Fails unless `path`, the KVM device, opens for reading and writing, as
/// Firecracker opens it. A host without KVM, or whose KVM the daemon's user
/// may not use, fails here instead of in a VM that exits at once.
pub fn check_kvm(path: &Path) -> Result<(), Error> {
  std::fs::OpenOptions::new().read(true).write(true).open(path).map(drop).map_err(|error| Error::Kvm {
    path: path.to_path_buf(),
    error,
  })
}

/// The files `needy build` writes for actors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
  pub kernel: PathBuf,
  pub rootfs: PathBuf,
  /// The patched Firecracker that applies a recipe's identity.
  pub firecracker: PathBuf,
}

impl Image {
  /// The image in directory `dir`.
  pub fn new(dir: &Path) -> Self {
    Self {
      kernel: dir.join("vmlinux"),
      rootfs: dir.join("rootfs.ext4"),
      firecracker: dir.join("firecracker"),
    }
  }

  /// Fails with the first file the image lacks.
  pub fn check(&self) -> Result<(), Error> {
    match [&self.kernel, &self.rootfs, &self.firecracker].into_iter().find(|path| !path.exists()) {
      Some(missing) => Err(Error::Missing(missing.clone())),
      None => Ok(()),
    }
  }
}

/// The vsock device's socket in a VM's directory.
pub fn vsock(dir: &Path) -> PathBuf {
  dir.join("v.sock")
}

/// Where the guest's vsock connections to host `port` arrive.
pub fn host_port(dir: &Path, port: u32) -> PathBuf {
  let mut name = vsock(dir).into_os_string();
  name.push(format!("_{port}"));
  PathBuf::from(name)
}

/// The guest's serial console, as Firecracker writes it: the actor's log.
pub fn console(dir: &Path) -> PathBuf {
  super::driver::log(dir)
}

/// The `linux-firecracker` driver: actors in microVMs booted from `image`, on
/// the KVM device `kvm`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxFirecracker {
  pub image: Image,
  /// [`KVM`] outside tests.
  pub kvm: PathBuf,
}

#[async_trait::async_trait]
impl Driver for LinuxFirecracker {
  fn name(&self) -> &'static str {
    "linux-firecracker"
  }

  async fn check_host(&self) -> Result<(), Error> {
    check_kvm(&self.kvm)
  }

  fn channels(&self, dir: &Path) -> Channels {
    let guest = host_port(dir, GUEST_PORT);
    let gateway = host_port(dir, GATEWAY_PORT);
    Channels {
      sockets: vec![vsock(dir), guest.clone(), gateway.clone()],
      guest: Endpoint::Unix(guest),
      gateway: Endpoint::Unix(gateway),
      terminal: Terminal::FirecrackerVsock {
        device: vsock(dir),
        port: PROCESS_PORT,
      },
      gateway_port: GATEWAY_PORT,
      process_port: PROCESS_PORT,
      // vsock has no file modes inside the VM.
      guest_for_every_user: false,
    }
  }

  fn setup(&self) -> Setup {
    Setup::Guest
  }

  async fn start(&self, start: &Start<'_>) -> Result<Box<dyn Running>, Error> {
    let launch = Launch::new(&self.image, start.dir, start.recipe);
    Ok(Box::new(Vm::start(&launch)?))
  }
}

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
    let (vcpus, mem_mib) = recipe.map(|r| (r.vcpus.max(1), r.mem_mib.max(256))).unwrap_or((2, 2048));
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

/// A running VM. The handle owns the Firecracker process; Firecracker exits
/// when the guest reboots after its driving program, or when the handle
/// stops it.
// NOTICE: the process is not killed when the handle is dropped. Whether a VM
// outlives a daemon restart is decided with persistence (slice 9 of the
// module layout redesign); until then a dropped handle leaves it running, as
// before this handle existed.
#[derive(Debug)]
pub struct Vm {
  child: tokio::process::Child,
  pid: u32,
  vcpus: u32,
  mem_mib: u32,
}

impl Vm {
  /// Writes `vm.json` and starts Firecracker with the guest's console in
  /// `console.log`.
  pub fn start(launch: &Launch) -> Result<Vm, Error> {
    let image = &launch.image;
    image.check()?;
    let config_path = launch.dir.join("vm.json");
    let write_error = |path: &Path| {
      let path = path.to_path_buf();
      move |source| Error::Write { path, source }
    };
    // NOTICE: a `Value` always serializes.
    let config = serde_json::to_vec_pretty(&launch.config).expect("a JSON value serializes");
    std::fs::write(&config_path, config).map_err(write_error(&config_path))?;
    let console_path = console(&launch.dir);
    let console = std::fs::File::create(&console_path).map_err(write_error(&console_path))?;
    let stderr = console.try_clone().map_err(write_error(&console_path))?;
    let child = tokio::process::Command::new(&image.firecracker)
      .args(&launch.args)
      .envs(&launch.env)
      .stdin(Stdio::null())
      .stdout(console)
      .stderr(stderr)
      .spawn()
      .map_err(Error::Spawn)?;
    // NOTICE: `id` is `None` only after the child was waited for.
    let pid = child.id().unwrap_or(0);
    Ok(Vm {
      child,
      pid,
      vcpus: launch.vcpus,
      mem_mib: launch.mem_mib,
    })
  }
}

#[async_trait::async_trait]
impl Running for Vm {
  /// The Firecracker process's ID.
  fn pid(&self) -> u32 {
    self.pid
  }

  fn vcpus(&self) -> u32 {
    self.vcpus
  }

  fn mem_mib(&self) -> u32 {
    self.mem_mib
  }

  /// Waits until Firecracker exits.
  async fn wait(&mut self) -> std::io::Result<ExitStatus> {
    self.child.wait().await
  }

  /// Stops Firecracker and waits until it is gone: `SIGTERM` first, then
  /// `SIGKILL` if it is still running after `grace`.
  async fn stop(&mut self, grace: Duration) -> std::io::Result<ExitStatus> {
    super::driver::stop_process(&mut self.child, "firecracker", grace).await
  }
}

#[cfg(all(test, unix))]
#[path = "firecracker_test.rs"]
mod tests;
