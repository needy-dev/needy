//! What this machine offers, for checking a recipe against the host it runs
//! on. Parsing lives in `needy_core::recipe::HostInfo`; this module reads the
//! machine.

use std::path::Path;

use needy_core::recipe::HostInfo;

/// This machine's CPU, and the CPUID entries its KVM supports. Other systems
/// than Linux report no flags, which skips the recipe's host checks.
pub fn detect() -> std::io::Result<HostInfo> {
  if cfg!(target_os = "linux") {
    let mut info = cpuinfo(Path::new("/proc/cpuinfo"))?;
    info.kvm_cpuid = kvm_cpuid();
    Ok(info)
  } else {
    Ok(HostInfo::default())
  }
}

/// The CPU described by a `/proc/cpuinfo` file, such as one saved from
/// another host.
pub fn cpuinfo(path: &Path) -> std::io::Result<HostInfo> {
  Ok(HostInfo::from_cpuinfo(&std::fs::read_to_string(path)?))
}

/// The `(leaf, subleaf)` entries `KVM_GET_SUPPORTED_CPUID` reports, read as
/// Firecracker reads them; `None` without a usable `/dev/kvm`.
// Source: https://github.com/firecracker-microvm/firecracker/blob/95f868c8e345b1cc8faccd1a3c910b4989dc3f58/src/vmm/src/arch/x86_64/kvm.rs#L53-L57
// Reference: https://www.kernel.org/doc/html/v6.12/virt/kvm/api.html#kvm-get-supported-cpuid
// NOTICE: Firecracker first asks for the AMX state permission
// (`request_dynamic_xstate_features`), which can add leaf 0xD subleaves; no
// recipe feature lives there, so the entries read here suffice.
// NOTICE: `KVM_GET_SUPPORTED_CPUID` is x86 only: kvm-ioctls 0.25.1 defines
// `get_supported_cpuid` under `#[cfg(target_arch = "x86_64")]`
// (`src/ioctls/system.rs` L419-L420) and kvm-bindings 0.14.2 defines
// `KVM_MAX_CPUID_ENTRIES` in `src/x86_64/fam_wrappers.rs` (L11), so an arm64
// build (the `vz` actor's guest) reports none, as other systems do.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn kvm_cpuid() -> Option<std::collections::BTreeSet<(u32, u32)>> {
  let kvm = match kvm_ioctls::Kvm::new() {
    Ok(kvm) => kvm,
    Err(e) => {
      tracing::debug!("cannot open /dev/kvm: {e}");
      return None;
    }
  };
  match kvm.get_supported_cpuid(kvm_bindings::KVM_MAX_CPUID_ENTRIES) {
    Ok(cpuid) => Some(cpuid.as_slice().iter().map(|entry| (entry.function, entry.index)).collect()),
    Err(e) => {
      tracing::warn!("cannot read KVM's supported CPUID: {e}");
      None
    }
  }
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn kvm_cpuid() -> Option<std::collections::BTreeSet<(u32, u32)>> {
  None
}

#[cfg(test)]
#[path = "detect_test.rs"]
mod tests;
