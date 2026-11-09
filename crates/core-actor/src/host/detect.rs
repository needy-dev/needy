//! What this machine offers, for checking a recipe against the host it runs
//! on. Parsing lives in `needy_core::recipe::HostInfo`; this module reads the
//! machine.

use std::path::Path;

use needy_core::recipe::HostInfo;

/// This machine's CPU. Other systems than Linux report no flags, which skips
/// the recipe's host checks.
pub fn detect() -> std::io::Result<HostInfo> {
  if cfg!(target_os = "linux") {
    cpuinfo(Path::new("/proc/cpuinfo"))
  } else {
    Ok(HostInfo::default())
  }
}

/// The CPU described by a `/proc/cpuinfo` file, such as one saved from
/// another host.
pub fn cpuinfo(path: &Path) -> std::io::Result<HostInfo> {
  Ok(HostInfo::from_cpuinfo(&std::fs::read_to_string(path)?))
}

#[cfg(test)]
#[path = "detect_test.rs"]
mod tests;
