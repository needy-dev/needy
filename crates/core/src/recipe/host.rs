//! What the host CPU offers, parsed from `/proc/cpuinfo` text.

use std::collections::BTreeSet;

/// What the host CPU offers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostInfo {
  pub flags: BTreeSet<String>,
  pub vendor: String,
  pub model_name: String,
}

impl HostInfo {
  /// Reads `/proc/cpuinfo`; other systems report no flags, which skips
  /// host checks.
  // TODO(core-actor): reading the host moves to `core-actor::host::detect` in
  // slice 6 of the module layout redesign; only the parsing stays here.
  pub fn detect() -> std::io::Result<HostInfo> {
    if cfg!(target_os = "linux") {
      Ok(Self::from_cpuinfo(&std::fs::read_to_string("/proc/cpuinfo")?))
    } else {
      Ok(HostInfo::default())
    }
  }

  pub fn from_cpuinfo(text: &str) -> HostInfo {
    let mut info = HostInfo::default();
    for line in text.lines() {
      let Some((key, value)) = line.split_once(':') else {
        continue;
      };
      let (key, value) = (key.trim(), value.trim());
      match key {
        "flags" if info.flags.is_empty() => {
          info.flags = value.split_whitespace().map(str::to_string).collect();
        }
        "vendor_id" if info.vendor.is_empty() => info.vendor = value.to_string(),
        "model name" if info.model_name.is_empty() => info.model_name = value.to_string(),
        _ => {}
      }
    }
    info
  }
}
