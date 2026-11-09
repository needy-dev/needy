//! What a session's trace records and where it goes.

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Trace {
  /// Where events go; empty records nothing.
  pub sinks: Vec<TraceSink>,
  /// Record per-hook latencies as `trace.timing` events.
  pub timing: bool,
}

impl Default for Trace {
  fn default() -> Self {
    Self {
      sinks: vec![TraceSink::Sqlite, TraceSink::Jsonl],
      timing: false,
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TraceSink {
  /// The daemon's `trace.db`, which `needy report` reads.
  Sqlite,
  /// `trace.jsonl` in the session's directory, for export.
  Jsonl,
  // TODO(trace): `otel` is deferred (module layout redesign, Hooks) until
  // the owner approves an exporter.
}
