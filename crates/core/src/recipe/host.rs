//! What the host CPU offers, parsed from `/proc/cpuinfo` text, and the
//! CPUID entries its KVM supports.

use std::collections::BTreeSet;

/// What the host CPU offers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostInfo {
  pub flags: BTreeSet<String>,
  pub vendor: String,
  pub model_name: String,
  /// The CPUID entries, as `(leaf, subleaf)`, that this host's KVM supports
  /// (`KVM_GET_SUPPORTED_CPUID`); `None` when they are unknown, such as on a
  /// host without KVM. Firecracker builds the guest's CPUID from exactly
  /// these entries.
  pub kvm_cpuid: Option<BTreeSet<(u32, u32)>>,
}

impl HostInfo {
  /// Parses `/proc/cpuinfo` text; the first processor's values count.
  /// `needy_core_actor::host::detect` reads the machine.
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
