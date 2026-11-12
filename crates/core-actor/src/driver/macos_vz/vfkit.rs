//! vfkit, the process that runs a `macos-vz` actor's VM: its program, the
//! check that it runs, and its command line from a compiled recipe.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use needy_core::recipe::CompiledRecipe;

use super::{Image, gateway_socket, guest_socket, pidfile, process_socket};
use crate::contract::{GATEWAY_PORT, GUEST_PORT, PROCESS_PORT};
use crate::driver::Error;

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
    let (vcpus, mem_mib) = crate::driver::size(recipe);
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
