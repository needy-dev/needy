//! The `linux-firecracker` actor driver: the actor's Firecracker VM, its
//! configuration from a compiled recipe ([`config`]), the KVM check and the
//! host CPU probe ([`kvm`]), and a handle that owns the Firecracker process.
//!
//! The image is a base image: the VM gets the guest needy on a second,
//! read-only disk (`crate::image::guest::disk`), which the boot stub mounts.
//!
//! A VM keeps its files in one directory: `vm.json` (the configuration),
//! `console.log` (the guest's serial console) and `v.sock` (the vsock device,
//! where guest-initiated connections to host port `P` land on `v.sock_P`).

pub mod config;
pub mod kvm;

use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

pub use config::Launch;
pub use kvm::{KVM, check_kvm};

use crate::contract::{GATEWAY_PORT, GUEST_PORT, PROCESS_PORT};
pub use crate::driver::Error;
use needy_config::profile::ActorDriver;
use needy_core::types::Capture;

#[cfg(unix)]
use crate::driver::Leftover;
use crate::driver::{Channels, Driver, Endpoint, Facets, Host, Identity, Running, Side, Start, Terminal, Transport};
pub use crate::image::vm::LinuxFirecracker as Image;

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
  crate::driver::log(dir)
}

/// What a `linux-firecracker` actor presents: needy's guest kernel catches
/// every exec, and init mounts the workspace and presents the recipe's
/// identity inside the VM, reached over vsock.
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

/// Whether this host can run `linux-firecracker` actors: Linux, a KVM device
/// that opens for reading and writing (as Firecracker opens it), and a
/// complete actor image.
pub fn detect(host: &Host) -> Result<(), String> {
  if !cfg!(target_os = "linux") {
    return Err("Firecracker runs on Linux only".into());
  }
  // The OS error alone, without the advice `check_kvm`'s error gives a run.
  if let Err(e) = std::fs::OpenOptions::new().read(true).write(true).open(&host.kvm) {
    return Err(format!("cannot open {}: {e}", host.kvm.display()));
  }
  Image::new(&host.image).check().map_err(|e| e.to_string())
}

/// Ends the Firecracker process a daemon that died left, found by its
/// `vm.json` on its command line.
#[cfg(unix)]
pub async fn reconcile(leftover: &Leftover<'_>) -> bool {
  crate::driver::end_vm(leftover, &leftover.dir.join("vm.json"), "firecracker").await
}

/// The `linux-firecracker` driver: actors in microVMs booted from `image`, on
/// the KVM device `kvm`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxFirecracker {
  pub image: Image,
  /// [`KVM`] outside tests.
  pub kvm: PathBuf,
  /// Where the guest disks are made ([`Host::guest_disks`]).
  pub guest_disks: PathBuf,
}

impl LinuxFirecracker {
  /// The driver on `host`: its KVM device and guest disks, and the VM image
  /// made from the OCI image `image` names (`actor.image`), or the base VM
  /// image without one.
  pub fn on(host: &Host, image: Option<&str>) -> Self {
    Self {
      image: crate::driver::vm_image(host, ActorDriver::LinuxFirecracker, image, Image::of),
      kvm: host.kvm.clone(),
      guest_disks: host.guest_disks.clone(),
    }
  }
}

#[async_trait::async_trait]
impl Driver for LinuxFirecracker {
  fn kind(&self) -> ActorDriver {
    ActorDriver::LinuxFirecracker
  }

  fn image(&self) -> String {
    crate::driver::describe_vm_image(&self.image.dir, &self.image.source)
  }

  async fn check_host(&self) -> Result<(), Error> {
    check_kvm(&self.kvm)?;
    self.image.check()
  }

  fn channels(&self, dir: &Path) -> Channels {
    let guest = host_port(dir, GUEST_PORT);
    let gateway = host_port(dir, GATEWAY_PORT);
    Channels {
      sockets: vec![vsock(dir), guest.clone(), gateway.clone()],
      guest: Endpoint::Unix(guest),
      gateway: Endpoint::Unix(gateway),
      terminal: Terminal::HybridVsock {
        device: vsock(dir),
        port: PROCESS_PORT,
      },
      gateway_port: GATEWAY_PORT,
      process_port: PROCESS_PORT,
    }
  }

  async fn start(&self, start: &Start<'_>) -> Result<Box<dyn Running>, Error> {
    let disk = crate::driver::guest_disk(start.guest, &self.guest_disks).await?;
    let launch = Launch::new(&self.image, start.dir, &disk, start.recipe);
    Ok(Box::new(Vm::start(&launch)?))
  }
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
      .map_err(|e| Error::driver(ActorDriver::LinuxFirecracker, format!("cannot start firecracker: {e}")))?;
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
    crate::driver::stop_process(&mut self.child, "firecracker", grace).await
  }
}

#[cfg(all(test, unix))]
#[path = "mod_test.rs"]
mod tests;
