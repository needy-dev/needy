//! The actor's init: the boot sequence, in order. It mounts the system, asks
//! the daemon for the run's configuration, sets up the identity, the
//! workspace mounts, the gateway forwarder, route stubs and copy-in, turns
//! routing on, runs the driving program, mirrors out, reports its exit and
//! reboots, which ends the VM.
//!
//! In a container (see [`super::in_container`]) the runtime made the system
//! mounts, the home directory, the hostname, the identity files and the
//! workspace mounts from the host, so init skips them, reaches the gateway
//! and serves the terminal on the bind-mounted sockets, turns routing on
//! for the shims, and exits instead of rebooting, which ends the container.
//!
//! Init logs to the console (the VM's serial port, the container's stderr),
//! which `needy run logs` prints.
//!
//! NOTICE: the tasks init spawns (the forwarder, sshfs bridges, scheduled
//! faults) are owned by the actor: they end when init reboots the VM or
//! exits the container.

mod driver;
mod files;
mod identity;
mod mounts;
mod network;
mod stubs;

pub(super) use stubs::STUB_DIR;

use std::time::Duration;

use anyhow::Result;
use needy_api_proto::v1::{GetConfigRequest, ReportExitRequest};

pub(super) fn main() -> ! {
  // Init started again by itself as a plain process (not PID 1) only
  // switches to the user and runs the driving program on its terminal.
  let args: Vec<String> = std::env::args().skip(1).collect();
  if std::process::id() != 1 && args.first().map(String::as_str) == Some(driver::AS_USER) {
    let error = driver::as_user(&args[1..]);
    eprintln!("needy init: cannot run the driving program: {error:#}");
    std::process::exit(127);
  }
  console_logging();
  let container = super::in_container();
  let booted = super::runtime().block_on(boot(container));
  if let Err(e) = &booted {
    tracing::error!("{e:#}");
  }
  if container {
    // The container ends with its PID 1; a failed boot says so in the
    // status the daemon reports.
    std::process::exit(if booted.is_ok() { 0 } else { 1 });
  }
  nix::unistd::sync();
  // NOTICE: how a guest ends its VM differs by architecture. On x86_64,
  // Firecracker ends on a reboot (`reboot=k` on the kernel command line,
  // https://github.com/firecracker-microvm/firecracker/blob/95f868c8e345b1cc8faccd1a3c910b4989dc3f58/FAQ.md#L181-L204)
  // and has no ACPI power-off. On arm64 a reboot is PSCI SYSTEM_RESET, which
  // Virtualization.framework answers by restarting the guest (init then
  // booted again and again in a macos-vz actor, seen 2026-11-12 with vfkit 0.6.4),
  // and a power-off is PSCI SYSTEM_OFF, which stops the VM and ends vfkit:
  // https://github.com/torvalds/linux/blob/v6.18/drivers/firmware/psci/psci.c#L309-L335
  let mode = if cfg!(target_arch = "aarch64") {
    nix::sys::reboot::RebootMode::RB_POWER_OFF
  } else {
    nix::sys::reboot::RebootMode::RB_AUTOBOOT
  };
  // `reboot` returns only when it fails.
  let Err(e) = nix::sys::reboot::reboot(mode);
  tracing::error!("cannot end the VM ({mode:?}): {e}");
  loop {
    std::thread::sleep(Duration::from_secs(60));
  }
}

/// Logs to init's stderr, the console. `NEEDY_LOG` on the kernel command
/// line takes `tracing` directives; the default is `info`.
fn console_logging() {
  let filter = tracing_subscriber::EnvFilter::try_from_env("NEEDY_LOG").unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
  let installed = tracing_subscriber::fmt()
    .with_writer(std::io::stderr)
    .with_ansi(false)
    .with_target(false)
    .with_timer(tracing_subscriber::fmt::time::uptime())
    .with_env_filter(filter)
    .try_init();
  if let Err(e) = installed {
    // Nothing else installs one in init; without it init runs unlogged.
    eprintln!("needy init: no console log: {e}");
  }
}

/// Writes a kernel parameter or sysfs value. A value the kernel lacks is
/// logged and skipped; the actor still boots.
fn write_param(path: &str, value: &str) {
  if let Err(e) = std::fs::write(path, value) {
    tracing::warn!("cannot write {path}: {e}");
  }
}

async fn boot(container: bool) -> Result<()> {
  // A container's runtime mounted the system and brought `lo` up, and its
  // `/proc/sys` is read-only.
  // TODO(container-actor): `vm.memfd_noexec` is not set in a container
  // actor, so its programs may execute code from a memfd. The setting is
  // per PID namespace, but init may not write `/proc/sys` there and Docker's
  // `sysctls` option is limited to the namespaced sysctls Docker validates;
  // setting it waits for a way the owner accepts (a privileged helper, or
  // a runtime that allows it), see the container actor runtime design.
  if !container {
    mounts::system();
    if let Err(e) = network::loopback_up() {
      tracing::warn!("cannot bring the loopback interface up: {e}");
    }
    // Programs may not execute code from a memfd.
    // Source: https://github.com/torvalds/linux/blob/v6.18/include/linux/pid_namespace.h#L21-L23
    // (2 = MEMFD_NOEXEC_SCOPE_NOEXEC_ENFORCED) and https://github.com/torvalds/linux/blob/v6.18/mm/memfd.c#L345-L366
    // (a `memfd_create` without flags gets MFD_NOEXEC_SEAL; MFD_EXEC fails with
    // EACCES).
    // Reference: https://github.com/torvalds/linux/blob/v6.18/Documentation/userspace-api/mfd_noexec.rst?plain=1#L54-L80
    write_param("/proc/sys/vm/memfd_noexec", "2");
  }

  let client = super::daemon().await?;
  let config = client.guest().get_config(GetConfigRequest {}).await.map_err(needy_api_client::Error::from)?.into_inner();
  let user = config.user.clone().unwrap_or_default();
  let home = format!("/home/{}", user.name);
  // A container's home is a tmpfs its runtime mounted; the daemon sends a
  // container no hostname, sensors or mounts, which its runtime set up.
  if !container {
    mounts::home(&home, user.uid, user.gid);
  }
  identity::apply(&config);
  for m in &config.mounts {
    let mount = crate::sshfs::Mount {
      target: m.target.clone(),
      executor: m.executor.clone(),
      source: m.source.clone(),
      writable: m.writable,
      sftp_command: m.sftp_command.clone(),
    };
    if let Err(e) = crate::sshfs::mount(&client, &mount, user.uid, user.gid).await {
      tracing::warn!("mount {}: {e:#}", m.target);
    }
  }
  let gateway = if container {
    network::Gateway::Unix(super::gateway_socket())
  } else {
    network::Gateway::Vsock(config.gateway_port)
  };
  tokio::spawn(network::forward_gateway(gateway));
  stubs::create(&config.route_names, container);
  for copy in &config.copy_in {
    if let Err(e) = files::copy_in(&client, &copy.name, &copy.path, user.uid, user.gid).await {
      tracing::warn!("copy-in {}: {e:#}", copy.name);
    }
  }
  identity::schedule_faults(&client, &config);
  // Route only after setup so init's own helpers stay local.
  if container {
    let route = super::shim::Route {
      uid: user.uid,
      names: config.route_names.clone(),
    };
    if let Err(e) = super::shim::enable(&route) {
      tracing::warn!("cannot turn routing on; every command stays in the actor: {e}");
    }
  } else {
    write_param("/sys/module/binfmt_needy/parameters/route_uid", &user.uid.to_string());
    write_param("/sys/module/binfmt_needy/parameters/route", &config.route_names.join(","));
  }

  let driven = match &config.tty {
    Some(window) => {
      let size = (u16::try_from(window.rows).unwrap_or(u16::MAX), u16::try_from(window.cols).unwrap_or(u16::MAX));
      let terminal = if container {
        driver::Terminal::Unix(super::process_socket())
      } else {
        driver::Terminal::Vsock(config.process_port)
      };
      driver::run_on_terminal(&config, &home, size, terminal).await.map(|exit| (exit.code, exit.signal))
    }
    None => driver::run(&config, &home).await.map(|status| {
      use std::os::unix::process::ExitStatusExt;
      (status.code(), status.signal().map(needy_core_executor::signals::name).unwrap_or_default())
    }),
  };
  let (code, signal) = match driven {
    Ok((code, signal)) => {
      tracing::info!("the driving program exited: code {code:?}, signal {signal:?}");
      (code, signal)
    }
    Err(e) => {
      tracing::error!("driving program: {e:#}");
      (Some(127), String::new())
    }
  };
  for copy in &config.mirror_out {
    if let Err(e) = files::mirror_out(&client, &copy.name, &copy.path).await {
      tracing::warn!("mirror-out {}: {e:#}", copy.name);
    }
  }
  if let Err(status) = client.guest().report_exit(ReportExitRequest { code, signal }).await {
    tracing::warn!("cannot report the driving program's exit: {}", status.message());
  }
  // Detach FUSE mounts before the streams behind them close. A container
  // has none; its runtime detaches its host-side mounts.
  mounts::detach(config.mounts.iter().map(|m| m.target.as_str()));
  Ok(())
}
