//! The actor's init: the boot sequence, in order. It mounts the system, asks
//! the daemon for the run's configuration, sets up the identity, the
//! workspace mounts, the gateway forwarder, route stubs and copy-in, turns
//! routing on, runs the driving program, mirrors out, reports its exit and
//! reboots, which ends the VM.
//!
//! Init logs to the console (the VM's serial port), which `needy run
//! logs` prints.
//!
//! NOTICE: the tasks init spawns (the forwarder, sshfs bridges, scheduled
//! faults) are owned by the VM: they end when init reboots it.

mod driver;
mod files;
mod identity;
mod mounts;
mod network;
mod sshfs;
mod stubs;

pub(super) use stubs::STUB_DIR;

use std::time::Duration;

use anyhow::Result;
use needy_api_proto::v1::{GetConfigRequest, ReportExitRequest};

pub(super) fn main() -> ! {
  console_logging();
  if let Err(e) = super::runtime().block_on(boot()) {
    tracing::error!("{e:#}");
  }
  nix::unistd::sync();
  // `reboot` returns only when it fails.
  let Err(e) = nix::sys::reboot::reboot(nix::sys::reboot::RebootMode::RB_AUTOBOOT);
  tracing::error!("cannot reboot: {e}");
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

async fn boot() -> Result<()> {
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

  let client = super::daemon().await?;
  let config = client.guest().get_config(GetConfigRequest {}).await.map_err(needy_api_client::Error::from)?.into_inner();
  let user = config.user.clone().unwrap_or_default();
  let home = format!("/home/{}", user.name);
  mounts::home(&home, user.uid, user.gid);
  identity::apply(&config);
  for m in &config.mounts {
    if let Err(e) = sshfs::mount(&client, m, user.uid, user.gid).await {
      tracing::warn!("mount {}: {e:#}", m.target);
    }
  }
  tokio::spawn(network::forward_gateway(config.gateway_port));
  stubs::create(&config.route_names);
  for copy in &config.copy_in {
    if let Err(e) = files::copy_in(&client, &copy.name, &copy.path, user.uid, user.gid).await {
      tracing::warn!("copy-in {}: {e:#}", copy.name);
    }
  }
  identity::schedule_faults(&client, &config);
  // Route only after setup so init's own helpers stay local.
  write_param("/sys/module/binfmt_needy/parameters/route_uid", &user.uid.to_string());
  write_param("/sys/module/binfmt_needy/parameters/route", &config.route_names.join(","));

  let (code, signal) = match driver::run(&config, &home).await {
    Ok(status) => {
      use std::os::unix::process::ExitStatusExt;
      tracing::info!("the driving program exited: {status}");
      (status.code(), status.signal().map(needy_core_executor::signals::name).unwrap_or_default())
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
  // Detach FUSE mounts before the streams behind them close.
  mounts::detach(config.mounts.iter().map(|m| m.target.as_str()));
  Ok(())
}
