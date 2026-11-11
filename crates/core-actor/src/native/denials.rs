//! The fence's denials as trace events. The kernel logs each operation a
//! Seatbelt profile denies; the fence tags its denials with the run's tag,
//! and `log stream` with a predicate on that tag reads them back.
//!
//! Source: Anthropic's sandbox-runtime reads denials the same way:
//! https://github.com/anthropic-experimental/sandbox-runtime/blob/41501a60b27deb89484b718acb76a04e2a37aa75/src/sandbox/macos-sandbox-utils.ts#L1640-L1650
//! Reference: `log(1)` (`man log`, macOS 27.2): `stream --style ndjson
//! --predicate`.

use std::process::Stdio;

use needy_api_client::Client;
use needy_api_proto::v1::{PushEventsRequest, TraceEvent};
use needy_core::event::{self, ActorDeny, Event};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// One denial from its log message: `Sandbox: NAME(PID) deny(N) OPERATION
/// TARGET`, then the profile's tag on the next line, and for a program that
/// is not Apple's a violation report after it. `None` for any other
/// message, such as `N duplicate reports for …`.
// NOTICE: the message format is the kernel's, seen on macOS 27.2:
// `Sandbox: bash(72949) deny(1) process-exec* /bin/ls`. For a program that
// is not a platform binary (opencode), `sandboxd` logs the same first two
// lines followed by a report (`Process: …`, `MetaData: {…}`), so the tag is
// not at the message's end.
pub(super) fn parse(message: &str) -> Option<ActorDeny> {
  let first = message.lines().next()?;
  let rest = first.strip_prefix("Sandbox: ")?;
  let (process, rest) = rest.split_once(" deny(")?;
  let (process, pid) = process.strip_suffix(')')?.rsplit_once('(')?;
  let (_, rest) = rest.split_once(") ")?;
  let (operation, target) = rest.split_once(' ').unwrap_or((rest, ""));
  Some(ActorDeny {
    process: process.to_string(),
    pid: pid.parse().ok()?,
    operation: operation.to_string(),
    target: target.to_string(),
  })
}

/// Starts reading the denials tagged `tag` and pushing each to the daemon
/// as an `actor.deny` event. The returned process is the reader; killing it
/// ends the task.
pub(super) fn watch(tag: &str, client: Client) -> std::io::Result<Child> {
  let mut child = Command::new("/usr/bin/log")
    .args([
      "stream",
      "--style",
      "ndjson",
      "--predicate",
      // `log`'s own message about this stream names the tag too; `parse`
      // drops it, since it is no denial.
      &format!("eventMessage CONTAINS \"{tag}\""),
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .kill_on_drop(true)
    .spawn()?;
  let Some(stdout) = child.stdout.take() else {
    return Ok(child);
  };
  // NOTICE: the task belongs to init's lifetime; it ends when `log` exits.
  tokio::spawn(async move {
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
      let Ok(entry) = serde_json::from_str::<serde_json::Value>(&line) else {
        continue;
      };
      let Some(deny) = entry.get("eventMessage").and_then(|m| m.as_str()).and_then(parse) else {
        continue;
      };
      tracing::info!("fence denied {} {} ({}[{}])", deny.operation, deny.target, deny.process, deny.pid);
      let event = Event::ActorDeny(deny);
      let pushed = TraceEvent {
        time_ms: needy_common::time::now_ms(),
        kind: event.kind().as_str().into(),
        payload: event.payload(),
        version: event::SCHEMA_VERSION,
      };
      if let Err(status) = client
        .guest()
        .push_events(tokio_stream::iter(vec![PushEventsRequest {
          event: Some(pushed),
        }]))
        .await
      {
        tracing::warn!("cannot record a fence denial: {}", status.message());
      }
    }
  });
  Ok(child)
}

#[cfg(test)]
#[path = "denials_test.rs"]
mod tests;
