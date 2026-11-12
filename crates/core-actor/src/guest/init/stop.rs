//! A graceful stop of the driving program: init holds `Guest.WaitStop`
//! while the program runs, and when `run stop` answers it, sends SIGTERM to
//! the program's process group, then SIGKILL after the grace period. The
//! program's exit then takes init's usual path: mirror-out, the exit report
//! and the end of the actor.

use std::time::Duration;

use needy_api_client::Client;
use needy_api_proto::v1::WaitStopRequest;

/// The wait for a stop; dropping it, once the driving program exited, ends
/// the wait and any signal still to come.
pub(super) struct Watch(tokio::task::JoinHandle<()>);

impl Drop for Watch {
  fn drop(&mut self) {
    self.0.abort();
  }
}

/// Sends the signal `name` to the process group `group` leads. A group
/// that is gone has exited already; its exit says how.
pub(super) fn signal_group(group: u32, name: &str) {
  use nix::sys::signal::{Signal, killpg};
  let (Ok(signal), Ok(group)) = (name.parse::<Signal>(), i32::try_from(group)) else {
    return;
  };
  if group <= 1 {
    return;
  }
  if let Err(e) = killpg(nix::unistd::Pid::from_raw(group), signal) {
    tracing::debug!("cannot send {name} to the driving program's group {group}: {e}");
  }
}

/// Waits for a stop of the driving program, which `signal` sends a signal
/// by name (`SIGTERM`, `SIGKILL`) to.
pub(super) fn watch(client: &Client, signal: impl Fn(&str) + Send + 'static) -> Watch {
  let client = client.clone();
  Watch(tokio::spawn(async move {
    let grace = match client.guest().wait_stop(WaitStopRequest {}).await {
      Ok(response) => Duration::from_millis(u64::try_from(response.into_inner().grace_ms).unwrap_or(0)),
      // The run ends without a graceful stop (the actor exits, or the
      // stop kills it): nothing to do.
      Err(status) => {
        tracing::debug!("no graceful stop: {}", status.message());
        return;
      }
    };
    tracing::info!("stopping the driving program: SIGTERM, then SIGKILL after {grace:?}");
    signal("SIGTERM");
    tokio::time::sleep(grace).await;
    tracing::info!("the driving program outlived its grace period: SIGKILL");
    signal("SIGKILL");
  }))
}
