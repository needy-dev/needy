//! Chaos: deterministic faults, fired from the seed, the fault's index and how
//! often it has matched.

use serde::Deserialize;

use crate::value::Duration;

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Chaos {
  pub seed: u64,
  pub faults: Vec<Fault>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fault {
  pub on: FaultKind,
  /// The subject pattern: a command pattern for `exec`, a glob on the host
  /// for `net.connect` and on the path for `file.open`. Absent matches
  /// everything.
  #[serde(rename = "match", default)]
  pub matcher: Option<String>,
  pub errno: Option<String>,
  pub delay: Option<Duration>,
  pub probability: Option<f64>,
  /// Fire only on this matching occurrence, counted from 1.
  pub nth: Option<u64>,
  /// Readings a `hwmon` fault sets.
  #[serde(default)]
  pub set: Readings,
  /// When a `hwmon` fault fires, after the actor starts.
  pub at: Option<Duration>,
}

/// Where a fault applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
pub enum FaultKind {
  #[serde(rename = "exec")]
  Exec,
  #[serde(rename = "net.connect")]
  NetConnect,
  #[serde(rename = "file.open")]
  FileOpen,
  #[serde(rename = "hwmon")]
  Hwmon,
}

impl FaultKind {
  /// The name users write in `on:`.
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Exec => "exec",
      Self::NetConnect => "net.connect",
      Self::FileOpen => "file.open",
      Self::Hwmon => "hwmon",
    }
  }
}

impl std::fmt::Display for FaultKind {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(self.as_str())
  }
}

/// Sensor readings a `hwmon` fault sets.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Readings {
  /// Package temperature in degrees Celsius.
  pub temp_c: Option<f64>,
}
