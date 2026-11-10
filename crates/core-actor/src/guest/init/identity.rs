//! The machine the driving program sees: the recipe's hostname and fake
//! sensors, scheduled sensor faults, and the uptime offset in a time
//! namespace.

use std::time::Duration;

use needy_api_client::Client;
use needy_api_proto::v1::{GetConfigResponse, PushEventsRequest, TraceEvent};
use needy_config::profile::FaultKind;
use needy_core::event::{self, Event};
use needy_core::types::{Effect, Fired};
use nix::sched::CloneFlags;

use super::write_param;

/// The built-in fake coretemp's parameters.
// Source: https://github.com/torvalds/linux/blob/v6.18/Documentation/ABI/stable/sysfs-module (built-in code with a
// parameter gets `/sys/module/<name>/parameters/`); `fake_coretemp.c` in
// `images/kernel/` declares `base_mc` and `override_mc` with mode 0644.
const CORETEMP: &str = "/sys/module/fake_coretemp/parameters";

/// Sets the hostname and the sensors' idle readings.
pub(super) fn apply(config: &GetConfigResponse) {
  if !config.hostname.is_empty()
    && let Err(e) = nix::unistd::sethostname(&config.hostname)
  {
    tracing::warn!("cannot set the hostname {}: {e}", config.hostname);
  }
  for sensor in &config.sensors {
    if sensor.chip == "coretemp" {
      write_param(&format!("{CORETEMP}/base_mc"), &sensor.idle_mc.to_string());
    }
  }
}

/// Schedules the profile's hwmon faults, each at its time after boot, and
/// records each in the run's trace when it fires.
pub(super) fn schedule_faults(client: &Client, config: &GetConfigResponse) {
  for fault in config.hwmon_faults.clone() {
    let client = client.clone();
    tokio::spawn(async move {
      tokio::time::sleep(Duration::from_millis(fault.at_ms.max(0) as u64)).await;
      write_param(&format!("{CORETEMP}/override_mc"), &fault.temp_mc.to_string());
      let fired = Event::FaultInjected(Fired {
        kind: FaultKind::Hwmon,
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
        tracing::warn!("cannot record the hwmon fault: {}", status.message());
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
