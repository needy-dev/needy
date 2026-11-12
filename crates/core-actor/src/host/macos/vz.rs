//! The `macos-vz` actor driver: needy's arm64 guest kernel and rootfs in a
//! Virtualization.framework VM on Apple silicon macOS, with vfkit as the VM
//! process, as Firecracker is on Linux.
//!
//! The guest is the Firecracker actor's in all that needy does inside it:
//! init is PID 1, `binfmt_needy` routes execs, the workspace is mounted with
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

use needy_config::recipe::IdentityField;
use needy_core::recipe::CompiledRecipe;

pub use crate::host::driver::Error;
use crate::host::driver::{Channels, Driver, Endpoint, Gap, Running, Setup, Start, Terminal};
pub use crate::host::firecracker::{GATEWAY_PORT, GUEST_PORT, PROCESS_PORT};

/// The VM process, looked up on `PATH`.
// NOTICE: Homebrew installs vfkit as `vfkit` (`brew install vfkit`,
// https://github.com/Homebrew/homebrew-core/blob/HEAD/Formula/v/vfkit.rb);
// a daemon started from a shell has Homebrew's directory on its `PATH`.
pub const VFKIT: &str = "vfkit";

/// The guest kernel's command line. `init=` starts needy's guest init.
// Reference: https://docs.kernel.org/6.18/admin-guide/kernel-parameters.html
// NOTICE: `console=hvc0` is the virtio console vfkit's `virtio-serial`
// device gives the guest (`VZVirtioConsoleDeviceSerialPortConfiguration`),
// https://github.com/crc-org/vfkit/blob/v0.6.4/pkg/vf/virtio.go#L273-L290.
// NOTICE: there is no `panic=`. Virtualization.framework restarts a guest
// that reboots (PSCI SYSTEM_RESET) instead of stopping the VM, so a panicked
// guest that rebooted would boot init again and could run the driving program
// twice. A panicked guest hangs instead (`CONFIG_PANIC_TIMEOUT=0` in
// `images/kernel/containerization-arm64.config`), with the panic on the
// console, until the run is stopped. Init ends the VM by powering off; see
// `crates/core-actor/src/guest/init/mod.rs`.
// TODO(macos-vz-panic): a pvpanic device would let the host see a panic and end
// the VM; Virtualization.framework has none, and vfkit cannot add one.
const BOOT_ARGS: &str = "console=hvc0 root=/dev/vda ro init=/.needy/init quiet loglevel=4";

/// The files `needy build` writes for `macos-vz` actors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
  /// The uncompressed arm64 kernel (`arch/arm64/boot/Image`):
  /// Virtualization.framework's Linux boot loader takes no compressed one.
  pub kernel: PathBuf,
  /// An empty initramfs: vfkit needs one (see `images/Dockerfile`).
  pub initrd: PathBuf,
  pub rootfs: PathBuf,
}

impl Image {
  /// The image in directory `dir`.
  pub fn new(dir: &Path) -> Self {
    Self {
      kernel: dir.join("Image"),
      initrd: dir.join("initrd"),
      rootfs: dir.join("rootfs.ext4"),
    }
  }

  /// Fails with the first file the image lacks.
  pub fn check(&self) -> Result<(), Error> {
    match [&self.kernel, &self.initrd, &self.rootfs].into_iter().find(|path| !path.exists()) {
      Some(missing) => Err(Error::Missing(missing.clone())),
      None => Ok(()),
    }
  }
}

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

/// Fails unless `vfkit` runs.
pub async fn check_vfkit(vfkit: &Path) -> Result<(), Error> {
  let output = tokio::process::Command::new(vfkit).arg("--version").stdin(Stdio::null()).output().await.map_err(|e| {
    Error::Unsupported(format!(
      "cannot run {}: {e}; a macos-vz actor needs vfkit (`brew install vfkit`, https://github.com/crc-org/vfkit)",
      vfkit.display()
    ))
  })?;
  if !output.status.success() {
    return Err(Error::Unsupported(format!(
      "{} --version failed ({}): {}",
      vfkit.display(),
      output.status,
      String::from_utf8_lossy(&output.stderr).trim()
    )));
  }
  Ok(())
}

/// The `macos-vz` driver: actors in Virtualization.framework VMs booted from
/// `image` by `vfkit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacosVz {
  pub image: Image,
  /// [`VFKIT`] outside tests.
  pub vfkit: PathBuf,
}

#[async_trait::async_trait]
impl Driver for MacosVz {
  fn name(&self) -> &'static str {
    "macos-vz"
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
      // vsock has no file modes inside the VM.
      guest_for_every_user: false,
    }
  }

  fn setup(&self) -> Setup {
    Setup::Guest
  }

  async fn start(&self, start: &Start<'_>) -> Result<Box<dyn Running>, Error> {
    let launch = Launch::new(&self.image, &self.vfkit, start.dir, start.recipe);
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

/// How one VM starts: vfkit's arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
  pub image: Image,
  /// The VM's directory.
  pub dir: PathBuf,
  pub vfkit: PathBuf,
  pub vcpus: u32,
  pub mem_mib: u32,
  pub args: Vec<String>,
}

impl Launch {
  /// The VM for `recipe`, or a plain 2 vCPU, 2 GiB VM without one.
  // NOTICE: vfkit splits a `--device` value at every comma, so a path with a
  // comma cannot be given; vfkit reports the option it cannot parse,
  // https://github.com/crc-org/vfkit/blob/v0.6.4/pkg/config/virtio.go#L181-L183.
  pub fn new(image: &Image, vfkit: &Path, dir: &Path, recipe: Option<&CompiledRecipe>) -> Self {
    let (vcpus, mem_mib) = recipe.map(|r| (r.vcpus.max(1), r.mem_mib.max(256))).unwrap_or((2, 2048));
    let path = |p: &Path| p.to_string_lossy().into_owned();
    let mut args = vec![
      "--cpus".to_string(),
      vcpus.to_string(),
      "--memory".into(),
      mem_mib.to_string(),
      // The command line is one CSV field, quoted, since it has spaces.
      "--bootloader".into(),
      format!("linux,kernel={},initrd={},\"cmdline={BOOT_ARGS}\"", path(&image.kernel), path(&image.initrd)),
      "--device".into(),
      format!("virtio-blk,path={},readonly", path(&image.rootfs)),
      // NOTICE: vfkit opens a log file without appending (`append: false`,
      // https://github.com/crc-org/vfkit/blob/v0.6.4/pkg/vf/virtio.go#L283),
      // which would write over its own messages in the same file; through
      // `/dev/stdout` the console shares vfkit's standard output, the
      // console.log the daemon opened for appending.
      "--device".into(),
      "virtio-serial,logFilePath=/dev/stdout".into(),
      "--device".into(),
      "virtio-rng".into(),
    ];
    // The guest dials the daemon's sockets on these ports; vfkit listens on
    // the process socket for the daemon and connects to the guest's port.
    for (port, socket, mode) in [
      (GUEST_PORT, guest_socket(dir), ""),
      (GATEWAY_PORT, gateway_socket(dir), ""),
      (PROCESS_PORT, process_socket(dir), ",connect"),
    ] {
      args.push("--device".into());
      args.push(format!("virtio-vsock,port={port},socketURL={}{mode}", path(&socket)));
    }
    // TODO(actor-network): no `virtio-net` device, as a Firecracker actor
    // has no network interface; see `firecracker::Launch`.
    args.extend([
      "--pidfile".into(),
      path(&pidfile(dir)),
      "--restful-uri".into(),
      "none://".into(),
    ]);
    Self {
      image: image.clone(),
      dir: dir.to_path_buf(),
      vfkit: vfkit.to_path_buf(),
      vcpus,
      mem_mib,
      args,
    }
  }
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
    let console_path = crate::host::driver::log(&launch.dir);
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
    crate::host::driver::stop_process(&mut self.child, "vfkit", grace).await
  }
}

#[cfg(all(test, unix))]
#[path = "vz_test.rs"]
mod tests;
