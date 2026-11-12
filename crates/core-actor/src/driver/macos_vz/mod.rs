//! The `macos-vz` actor driver: needy's arm64 guest kernel and rootfs in a
//! Virtualization.framework VM on Apple silicon macOS, with vfkit as the VM
//! process, as Firecracker is on Linux.
//!
//! The guest is the Firecracker actor's in all that needy does inside it:
//! the boot stub is PID 1 and starts the guest needy from the second disk
//! (`crate::image::guest::disk`) as init, `binfmt_needy` routes execs, the
//! workspace is mounted with
//! sshfs in the guest, and the guest API, gateway and process service are
//! vsock ports. macOS has no host `AF_VSOCK`, so vfkit bridges each port to a
//! Unix socket in the VM's directory: `guest.sock` and `gateway.sock`, which
//! the daemon serves and the guest dials, and `process.sock`, which vfkit
//! serves and connects to the guest's process service. The VM has no network
//! device; its traffic goes through the gateway.
//!
//! A VM keeps its files in one directory: `console.log` (vfkit's output and
//! the guest's console), `vfkit.pid`, and the three sockets.
//!
//! NOTICE: vfkit, not a helper of needy's own, owns the VM: it is maintained
//! (crc-org, used by Podman and CRC), carries the one entitlement
//! Virtualization.framework needs (`com.apple.security.virtualization`,
//! ad-hoc signed by its build), and already bridges vsock to Unix sockets.
//! A helper of needy's own (Swift, or `objc2-virtualization` in the needy
//! binary, which would then need the entitlement) is worth it only for what
//! vfkit cannot do; see
//! `docs/ai/references/architecture/2026-11-12-macos-vz-actor-evidence.md`.
// Reference: https://github.com/crc-org/vfkit/blob/v0.6.4/doc/usage.md

use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

pub mod vfkit;

use needy_config::profile::ActorDriver;
use needy_config::recipe::IdentityField;
use needy_core::recipe::CompiledRecipe;
use needy_core::types::Capture;

pub use vfkit::{Launch, VFKIT, check_vfkit};

use crate::contract::{GATEWAY_PORT, PROCESS_PORT};
pub use crate::driver::Error;
#[cfg(unix)]
use crate::driver::Leftover;
use crate::driver::{Channels, Driver, Endpoint, Facets, Gap, Host, Identity, Running, Side, Start, Terminal, Transport};
pub use crate::image::vm::MacosVz as Image;

/// Where the daemon serves the guest API in a VM's directory.
pub fn guest_socket(dir: &Path) -> PathBuf {
  dir.join("guest.sock")
}

/// Where the daemon serves the gateway in a VM's directory.
pub fn gateway_socket(dir: &Path) -> PathBuf {
  dir.join("gateway.sock")
}

/// Where vfkit serves the guest's process service in a VM's directory.
pub fn process_socket(dir: &Path) -> PathBuf {
  dir.join("process.sock")
}

/// The file vfkit writes its process ID to. A restarted daemon finds a
/// leftover VM by this path on vfkit's command line.
pub fn pidfile(dir: &Path) -> PathBuf {
  dir.join("vfkit.pid")
}

/// Whether this machine can run Virtualization.framework arm64 guests:
/// macOS on Apple silicon.
pub fn supported_host() -> bool {
  cfg!(all(target_os = "macos", target_arch = "aarch64"))
}

/// What a `macos-vz` actor presents: the Firecracker actor's guest, so the
/// kernel catches every exec and init mounts and presents the identity
/// inside, reached over vsock that vfkit bridges to Unix sockets.
pub const FACETS: Facets = Facets {
  capture: Capture::Kernel,
  workspace: Side::Guest,
  identity: Identity {
    hostname: Some(Side::Guest),
    hwmon: Some(Side::Guest),
    uptime: Some(Side::Guest),
  },
  transport: Transport::Vsock,
};

/// Whether this host can run `macos-vz` actors: [`detect_host`], and a
/// complete base VM image.
pub async fn detect(host: &Host) -> Result<(), String> {
  detect_host().await?;
  Image::new(&host.image).check().map_err(|e| e.to_string())
}

/// Whether this host could run `macos-vz` actors, whatever images it has:
/// macOS on Apple silicon, and a `vfkit` that runs.
pub async fn detect_host() -> Result<(), String> {
  if !supported_host() {
    return Err("Virtualization.framework actors run on macOS on Apple silicon only".into());
  }
  check_vfkit(Path::new(VFKIT)).await.map_err(|e| e.to_string())
}

/// Ends the vfkit process a daemon that died left, found by its pidfile on
/// its command line.
#[cfg(unix)]
pub async fn reconcile(leftover: &Leftover<'_>) -> bool {
  crate::driver::end_vm(leftover, &pidfile(leftover.dir), "vfkit").await
}

/// The `macos-vz` driver: actors in Virtualization.framework VMs booted from
/// `image` by `vfkit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacosVz {
  pub image: Image,
  /// [`VFKIT`] outside tests.
  pub vfkit: PathBuf,
  /// Where the guest disks are made ([`Host::guest_disks`]).
  pub guest_disks: PathBuf,
}

impl MacosVz {
  /// The driver on `host`: its guest disks, vfkit on `PATH`, and the VM
  /// image made from the OCI image `image` names (`actor.image`), or the
  /// base VM image without one.
  pub fn on(host: &Host, image: Option<&str>) -> Self {
    Self {
      image: crate::driver::vm_image(host, ActorDriver::MacosVz, image, Image::of),
      vfkit: VFKIT.into(),
      guest_disks: host.guest_disks.clone(),
    }
  }
}

#[async_trait::async_trait]
impl Driver for MacosVz {
  fn kind(&self) -> ActorDriver {
    ActorDriver::MacosVz
  }

  fn image(&self) -> String {
    crate::driver::describe_vm_image(&self.image.dir, &self.image.source)
  }

  async fn check_host(&self) -> Result<(), Error> {
    if !supported_host() {
      return Err(Error::Unsupported(
        "a macos-vz actor runs on macOS on Apple silicon only; on Linux use `driver: linux-firecracker` or `driver: linux-container`".into(),
      ));
    }
    check_vfkit(&self.vfkit).await?;
    self.image.check()
  }

  fn channels(&self, dir: &Path) -> Channels {
    let (guest, gateway, process) = (guest_socket(dir), gateway_socket(dir), process_socket(dir));
    Channels {
      sockets: vec![guest.clone(), gateway.clone(), process.clone()],
      guest: Endpoint::Unix(guest),
      gateway: Endpoint::Unix(gateway),
      terminal: Terminal::Unix(process),
      gateway_port: GATEWAY_PORT,
      process_port: PROCESS_PORT,
    }
  }

  async fn start(&self, start: &Start<'_>) -> Result<Box<dyn Running>, Error> {
    let disk = crate::driver::guest_disk(start.guest, &self.guest_disks).await?;
    let launch = Launch::new(&self.image, &self.vfkit, start.dir, &disk, start.recipe);
    Ok(Box::new(Vm::start(&launch)?))
  }

  fn identity_gaps(&self, recipe: Option<&CompiledRecipe>) -> Vec<Gap> {
    gaps(recipe)
  }
}

/// What of `recipe` a `macos-vz` actor does not present. The guest kernel, init
/// and rootfs are the Firecracker actor's, so the hostname, the sensors and
/// their faults, the kernel identity and the uptime offset are there; what
/// the VM itself would have to fake is not.
pub fn gaps(recipe: Option<&CompiledRecipe>) -> Vec<Gap> {
  let Some(recipe) = recipe else {
    return Vec::new();
  };
  // NOTICE: needy's recipes describe x86_64 machines (a CPU brand, a family,
  // model and stepping, x86 flag names, SMBIOS, Intel PCI bridges); no recipe
  // field names an architecture, so every recipe is x86 here.
  // TODO(macos-vz-identity): arm64 recipes, or a recipe `arch`, would let a macos-vz
  // actor be faithful; deferred until the owner asks for arm64 machines.
  let mut gaps = vec![Gap {
    field: None,
    message: "the recipe describes an x86_64 machine and a macos-vz actor is arm64 (Apple silicon): uname -m is aarch64 and \
              programs are arm64 ELF"
      .into(),
  }];
  let cpu = &recipe.cpu;
  if cpu.brand.is_some() || cpu.signature.is_some() || !cpu.hide.is_empty() || cpu.hypervisor.is_some() {
    // NOTICE: Virtualization.framework has no API for the guest's MIDR or
    // ID registers: `VZGenericPlatformConfiguration` has only a machine
    // identifier and nested virtualization (macOS 27.2 SDK,
    // `Virtualization.framework/Headers/VZGenericPlatformConfiguration.h`).
    // TODO(macos-vz-identity): a generated arm64 `/proc/cpuinfo` mounted over the
    // kernel's by init, as the `linux-container` driver mounts its x86 one, could
    // carry the recipe's CPU count; it could not carry an x86 brand.
    gaps.push(Gap::of(
      IdentityField::Cpuid,
      "the CPU is the host's Apple arm64 core; Virtualization.framework cannot set the guest's ID registers (MIDR)",
    ));
    gaps.push(Gap::of(
      IdentityField::Cpuinfo,
      "/proc/cpuinfo is arm64's (CPU implementer 0x61, Apple), without the recipe's brand, signature or flags",
    ));
  }
  if recipe.identity.smbios.is_some() {
    // NOTICE: the macOS 27.2 SDK's Virtualization.framework headers have no
    // SMBIOS, serial or product-name API (`grep -ri smbios` finds nothing),
    // and its Linux boot loader starts the kernel with a device tree, not
    // firmware tables.
    // TODO(macos-vz-identity): a fake DMI module in the guest kernel, like
    // fake_coretemp, could make `/sys/class/dmi/id`; not approved yet.
    gaps.push(Gap::of(IdentityField::Dmi, "Virtualization.framework gives the guest no SMBIOS, so it has no /sys/class/dmi/id"));
  }
  if recipe.identity.has_pci() {
    gaps.push(Gap::of(IdentityField::Pci, "PCI IDs are Virtualization.framework's virtio devices; it cannot set them"));
  }
  gaps
}

/// A running VM. The handle owns the vfkit process; vfkit exits when the
/// guest reboots after its driving program, or when the handle stops it.
// NOTICE: as with Firecracker's `Vm`, the process is not killed when the
// handle is dropped; a restarted daemon ends a leftover VM (`reconcile`).
#[derive(Debug)]
pub struct Vm {
  child: tokio::process::Child,
  pid: u32,
  vcpus: u32,
  mem_mib: u32,
}

impl Vm {
  /// Starts vfkit with its output and the guest's console in `console.log`.
  pub fn start(launch: &Launch) -> Result<Vm, Error> {
    launch.image.check()?;
    let console_path = crate::driver::log(&launch.dir);
    let write_error = |source| Error::Write {
      path: console_path.clone(),
      source,
    };
    let console = std::fs::OpenOptions::new().create(true).write(true).truncate(true).open(&console_path).map_err(write_error)?;
    drop(console);
    // Appending, so the guest's console and vfkit's messages do not write
    // over each other.
    let console = std::fs::OpenOptions::new().append(true).open(&console_path).map_err(write_error)?;
    let stderr = console.try_clone().map_err(write_error)?;
    let child = tokio::process::Command::new(&launch.vfkit)
      .args(&launch.args)
      .stdin(Stdio::null())
      .stdout(console)
      .stderr(stderr)
      .spawn()
      .map_err(|e| Error::Unsupported(format!("cannot start {}: {e}", launch.vfkit.display())))?;
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
  /// The vfkit process's ID.
  fn pid(&self) -> u32 {
    self.pid
  }

  fn vcpus(&self) -> u32 {
    self.vcpus
  }

  fn mem_mib(&self) -> u32 {
    self.mem_mib
  }

  /// Waits until vfkit exits.
  async fn wait(&mut self) -> std::io::Result<ExitStatus> {
    self.child.wait().await
  }

  /// Stops vfkit and waits until it is gone: `SIGTERM` (vfkit asks the
  /// guest to stop, then stops the VM) first, then `SIGKILL` after `grace`.
  async fn stop(&mut self, grace: Duration) -> std::io::Result<ExitStatus> {
    crate::driver::stop_process(&mut self.child, "vfkit", grace).await
  }
}

#[cfg(all(test, unix))]
#[path = "mod_test.rs"]
mod tests;
