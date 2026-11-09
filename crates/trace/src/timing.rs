//! Per-hook latency: how long each hook point took in the flow, as its
//! producer measured it. Dispatch sums the samples and writes them as one
//! `trace.timing` event when it flushes or closes.
//
// TODO(trace): percentiles need a histogram per hook; `xtask bench` computes
// its own p50 and p99 from probe output, so the trace keeps count, total,
// minimum and maximum until a reader needs more.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Serialize;

#[derive(Debug, Default, Serialize)]
struct Stats {
  count: u64,
  total_us: u64,
  min_us: u64,
  max_us: u64,
}

/// Samples since the last summary, by hook.
#[derive(Debug, Default)]
pub(crate) struct Timing {
  hooks: BTreeMap<String, Stats>,
}

#[derive(Serialize)]
struct Summary<'a> {
  hooks: &'a BTreeMap<String, Stats>,
}

impl Timing {
  pub(crate) fn add(&mut self, hook: &str, elapsed: Duration) {
    let us = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
    let stats = self.hooks.entry(hook.to_string()).or_default();
    stats.min_us = if stats.count == 0 {
      us
    } else {
      stats.min_us.min(us)
    };
    stats.max_us = stats.max_us.max(us);
    stats.total_us = stats.total_us.saturating_add(us);
    stats.count += 1;
  }

  /// The `trace.timing` payload of the samples so far, which it clears;
  /// `None` when there are none.
  pub(crate) fn take(&mut self) -> Option<Box<serde_json::value::RawValue>> {
    if self.hooks.is_empty() {
      return None;
    }
    let payload = serde_json::value::to_raw_value(&Summary { hooks: &self.hooks });
    self.hooks.clear();
    // NOTICE: a map of strings to integer structs always serializes.
    Some(payload.expect("timing summaries serialize to JSON"))
  }
}
