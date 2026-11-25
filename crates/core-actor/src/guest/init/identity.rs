//! The machine the driving program sees: the recipe's hostname and fake
//! sensors, scheduled sensor faults, and the uptime offset in a time
//! namespace.

use std::path::Path;
use std::time::Duration;

use needy_api_client::Client;
use needy_api_proto::v1::{GetConfigResponse, PushEventsRequest, TraceEvent};
use needy_config::profile::FaultDomain;
use needy_core::event::{self, Event};
use needy_core::types::{Effect, Fired};
use nix::mount::MsFlags;
use nix::sched::CloneFlags;

use super::write_param;

/// Where init keeps rewritten identity files on the `/run` tmpfs before
/// bind-mounting them over read-only `/etc` paths (same pattern as `account`).
const DIR: &str = "/run/needy/identity";

/// The built-in fake coretemp's parameters.
// Source: https://github.com/torvalds/linux/blob/v6.18/Documentation/ABI/stable/sysfs-module (built-in code with a
// parameter gets `/sys/module/<name>/parameters/`); `fake_coretemp.c` in
// `images/kernel/` declares `base_mc` and `override_mc` with mode 0644.
const CORETEMP: &str = "/sys/module/fake_coretemp/parameters";

/// Sets the hostname, `/etc/machine-id`, and the sensors' idle readings.
pub(super) fn apply(config: &GetConfigResponse) {
  if !config.hostname.is_empty()
    && let Err(e) = nix::unistd::sethostname(&config.hostname)
  {
    tracing::warn!("cannot set the hostname {}: {e}", config.hostname);
  }
  if !config.machine_id.is_empty()
    && let Err(e) = apply_machine_id(&config.machine_id)
  {
    tracing::warn!("cannot set /etc/machine-id: {e}");
  }
  for sensor in &config.sensors {
    if sensor.chip == "coretemp" {
      write_param(&format!("{CORETEMP}/base_mc"), &sensor.idle_mc.to_string());
    }
  }
}

/// Applies `/etc/machine-id`. Skips when already equal (e.g. host-bound by
/// `linux-container`); otherwise writes directly on writable rootfs or falls
/// back to writing `/run/needy/identity/machine-id` on tmpfs and bind-mounting
/// it over `/etc/machine-id` for read-only VM rootfs.
fn apply_machine_id(machine_id: &str) -> std::io::Result<()> {
  use std::os::unix::fs::PermissionsExt;

  let target = Path::new("/etc/machine-id");
  let content = format!("{machine_id}\n");
  if std::fs::read_to_string(target).is_ok_and(|current| current == content) {
    return Ok(());
  }
  if std::fs::write(target, &content).is_ok() {
    return Ok(());
  }
  std::fs::create_dir_all(DIR)?;
  let source = Path::new(DIR).join("machine-id");
  std::fs::write(&source, &content)?;
  std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o444))?;
  nix::mount::mount(Some(&source), target, None::<&str>, MsFlags::MS_BIND, None::<&str>)?;
  Ok(())
}

/// Schedules the profile's sensor faults, each at its time after boot, and
/// records each in the run's trace when it fires.
pub(super) fn schedule_faults(client: &Client, config: &GetConfigResponse) {
  for fault in config.sensor_faults.clone() {
    let client = client.clone();
    tokio::spawn(async move {
      tokio::time::sleep(Duration::from_millis(fault.at_ms.max(0) as u64)).await;
      write_param(&format!("{CORETEMP}/override_mc"), &fault.temp_mc.to_string());
      let fired = Event::FaultInjected(Fired {
        name: fault.name.clone(),
        domain: FaultDomain::Sensor,
        target: "coretemp".into(),
        effect: Effect {
          temp_mc: Some(fault.temp_mc),
          ..Effect::default()
        },
      });
      let event = TraceEvent {
        time_ms: needy_common::time::now_ms(),
        kind: fired.kind().as_str().into(),
        payload: fired.payload(),
        version: event::SCHEMA_VERSION,
      };
      if let Err(status) = client.guest().push_events(tokio_stream::iter(vec![PushEventsRequest { event: Some(event) }])).await {
        tracing::warn!("cannot record the sensor fault {}: {}", fault.name, status.message());
      }
    });
  }
}

/// Moves init's future children into a time namespace whose monotonic and
/// boot clocks are `offset_secs` ahead, so the driving program sees the
/// recipe's uptime. Init itself stays in the boot namespace.
pub(super) fn offset_uptime(offset_secs: i64) {
  if offset_secs == 0 {
    return;
  }
  // NOTICE: nix 0.30 has no `CLONE_NEWTIME` constant (Linux 5.6); libc does.
  if let Err(e) = nix::sched::unshare(CloneFlags::from_bits_retain(libc::CLONE_NEWTIME)) {
    tracing::warn!("no time namespace; the uptime is the VM's: {e}");
    return;
  }
  // Offsets are written once, before the namespace's first process.
  // Source: https://github.com/torvalds/linux/blob/v6.18/fs/proc/base.c#L1671-L1684 (`<clock> <secs> <nanosecs>` per
  // line, clock `monotonic` or `boottime`) and
  // https://github.com/torvalds/linux/blob/v6.18/kernel/time/namespace.c#L383-L461 (refused once the namespace has a
  // process).
  // Reference: https://man7.org/linux/man-pages/man7/time_namespaces.7.html
  // (man-pages 6.19, read 2026-11-10).
  write_param("/proc/self/timens_offsets", &format!("monotonic {offset_secs} 0\nboottime {offset_secs} 0\n"));
}
