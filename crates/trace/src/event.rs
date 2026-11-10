//! The envelope every event travels in. The payload is opaque JSON; its
//! schema belongs to the producer (`needy_core::event`).

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

/// The kind of the event Dispatch writes when it dropped events: payload
/// `{"dropped": N}`.
pub const GAP: &str = "trace.gap";
/// The kind of the event Dispatch writes with per-hook latencies: payload
/// `{"hooks": {HOOK: {"count", "total_us", "min_us", "max_us"}}}`.
pub const TIMING: &str = "trace.timing";

/// One event of one run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
  pub run: String,
  /// The event's place in its run, from 0, in the order Dispatch
  /// accepted it. Dropped events take no number; a `trace.gap` marks them.
  pub seq: u64,
  /// Milliseconds since the Unix epoch.
  pub time_ms: i64,
  /// For example `exec.decide`.
  pub kind: String,
  /// The payload schema version, as the producer set it on its Dispatch.
  pub version: u32,
  /// A JSON object.
  pub payload: Box<RawValue>,
}
