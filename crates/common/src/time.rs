//! Wall-clock time as needy records it.

use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch; 0 when the clock is before it.
pub fn now_ms() -> i64 {
  SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}
