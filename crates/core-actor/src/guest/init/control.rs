//! The control stream init holds for the actor's life
//! (`GuestService.Control`): what the daemon tells a running actor. A route
//! list, which `needy chaos add` and `rm` change, is applied to the capture
//! as it arrives, with a route stub for each new command the actor lacks; a
//! graceful stop goes to the driving program's watch (`super::stop`).

use std::time::Duration;

use needy_api_client::Client;
use needy_api_proto::v1::GuestServiceControlRequest;
use needy_api_proto::v1::guest_service_control_response::Message;

use super::stubs;
use crate::guest::capture::{self, Route};

/// Follows the daemon's control stream until it ends with the run, applying
/// each route list for user `uid` and passing a graceful stop to `stops`. A
/// daemon that cannot be followed leaves the route list init booted with,
/// and the driving program without a graceful stop.
pub(super) async fn follow(client: Client, uid: u32, container: bool, stops: tokio::sync::watch::Sender<Option<Duration>>) {
  let mut stream = match client.guest().control(GuestServiceControlRequest {}).await {
    Ok(response) => response.into_inner(),
    Err(status) => {
      tracing::warn!("the route list stays as it booted: cannot follow the daemon's control stream: {}", status.message());
      return;
    }
  };
  loop {
    match stream.message().await {
      Ok(Some(control)) => match control.message {
        Some(Message::Routes(list)) => apply(
          Route {
            uid,
            names: list.names,
          },
          container,
        ),
        Some(Message::Stop(stop)) => {
          stops.send_replace(Some(Duration::from_millis(u64::try_from(stop.grace_ms).unwrap_or(0))));
        }
        // NOTICE: a message this build does not know, from a newer daemon,
        // is skipped rather than ending the stream.
        None => tracing::debug!("skipping a control message this guest does not know"),
      },
      Ok(None) => return,
      Err(status) => {
        tracing::warn!("the daemon's control stream failed; the route list stays as it is: {}", status.message());
        return;
      }
    }
  }
}

/// Routes `route` from now on: stubs first, so a newly routed command the
/// actor lacks exists by the time the capture routes it.
fn apply(route: Route, container: bool) {
  tracing::info!("route list: {}", route.names.join(","));
  stubs::create(&route.names, container);
  capture::enable(&route, container);
}
