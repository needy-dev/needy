//! Recipe compilation: one machine's identity, compiled for a host and a
//! run seed. Recipes themselves are loaded by `needy-config`.

mod cpu;
pub mod files;
mod host;
mod identity;
mod kernel;

use std::collections::BTreeMap;
use std::time::{Duration, UNIX_EPOCH};

use needy_config::recipe::{Hypervisor, Recipe};
use needy_config::validate::Finding;
use serde_json::json;

pub use host::HostInfo;
pub use identity::{PciId, Smbios};
use identity::{fill_placeholders, pci_id};

#[derive(Debug, Clone, PartialEq)]
pub struct SensorSpec {
  pub chip: String,
  pub idle_mc: i32,
  pub load_gain_mc: i32,
  pub noise_mc: i32,
  pub per_core: bool,
}

/// What the VM presents about itself through the hypervisor: CPU brand,
/// the hypervisor bit, SMBIOS and PCI bridges.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VmIdentity {
  pub cpu_brand: Option<String>,
  /// Clears the CPUID hypervisor bit and the KVM leaves.
  pub hide_hypervisor: bool,
  pub smbios: Option<Smbios>,
  pub pci_host_bridge: Option<PciId>,
  pub pci_isa_bridge: Option<PciId>,
}

impl VmIdentity {
  /// Whether the VM needs a PCI bus for the identity's bridges.
  pub fn has_pci(&self) -> bool {
    self.pci_host_bridge.is_some() || self.pci_isa_bridge.is_some()
  }
}

/// What the recipe says of its CPU as `/proc/cpuinfo` shows it, for a
/// container actor, whose identity files stand in for the CPUID a VM's
/// patched Firecracker presents.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CpuView {
  pub brand: Option<String>,
  /// Family, model and stepping.
  pub signature: Option<(u32, u32, u32)>,
  /// `/proc/cpuinfo` flag names to leave out.
  pub hide: Vec<String>,
  /// Whether the `hypervisor` flag shows; `None` keeps the host's.
  pub hypervisor: Option<bool>,
  pub threads_per_core: u32,
}

/// What a run's own recipe values are generated from, so a replayed run,
/// given the same, gets the same machine: the seed (from the run's ID)
/// fills `{hash}`, `{seq}` and the system UUID, and `time` fills `{time}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerRun {
  pub seed: u64,
  /// When the run was created, in UTC, as `YYYYMMDD-HHMMSS`: the suffix of
  /// the default name the GCE Console gives a VM. The trace records it in
  /// `run.create` (`recipe_time`).
  pub time: String,
}

impl PerRun {
  /// The values of a run created at `created_ms` (milliseconds since the
  /// Unix epoch).
  pub fn at(seed: u64, created_ms: i64) -> Self {
    Self {
      seed,
      time: time(created_ms),
    }
  }
}

/// `created_ms` in UTC as `YYYYMMDD-HHMMSS`: the digits of its RFC 3339
/// form (`2026-11-12T01:02:03Z`), which `humantime` writes.
fn time(created_ms: i64) -> String {
  let at = UNIX_EPOCH + Duration::from_millis(u64::try_from(created_ms).unwrap_or(0));
  let digits: String = humantime::format_rfc3339_seconds(at).to_string().chars().filter(char::is_ascii_digit).collect();
  format!("{}-{}", &digits[..8], &digits[8..14])
}

/// A recipe compiled for one host and run.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledRecipe {
  pub vcpus: u32,
  pub mem_mib: u32,
  pub identity: VmIdentity,
  pub cpu: CpuView,
  /// Firecracker custom CPU template.
  pub cpu_template: serde_json::Value,
  /// Make variables for building the guest kernel, passed to
  /// `images/Dockerfile` as build arguments of its `kernel` stage.
  pub kernel_build_env: BTreeMap<String, String>,
  pub hostname: Option<String>,
  /// The `{time}` it was compiled with ([`PerRun::time`]).
  pub time: String,
  pub uptime_offset_secs: i64,
  pub sensors: Vec<SensorSpec>,
  /// TODO(actor-network): checked against the identity's vendor only; no
  /// actor has a NIC to carry it (see `needy_core_actor::driver::linux_firecracker::Launch`).
  pub network_mac: Option<String>,
  pub interface: Option<String>,
  pub metadata: Option<serde_json::Value>,
  pub os_image: Option<String>,
  /// The identity fields the actor must present (`required:`).
  pub required: Vec<needy_config::recipe::IdentityField>,
  pub findings: Vec<Finding>,
}

/// Compiles a recipe for `host`; `run` fills the placeholders and every
/// other per-run value, so a replayed run gets the same machine.
pub fn compile(recipe: &Recipe, host: &HostInfo, run: &PerRun) -> CompiledRecipe {
  let mut findings = Vec::new();
  let fill = |text: &str| fill_placeholders(text, run);

  let topology = recipe.cpu.topology.clone();
  let vcpus = recipe.vcpus.or_else(|| topology.map(|t| t.cores.max(1) * t.threads_per_core.max(1))).unwrap_or(2);
  let mem_mib = match recipe.memory {
    None => 1024,
    Some(size) => u32::try_from(size.mib()).unwrap_or_else(|_| {
      findings.push(Finding::warning(format!("memory of {} MiB is too large; using 1024 MiB", size.mib())));
      1024
    }),
  };

  let hidden = recipe.cpu.hypervisor == Some(Hypervisor::Hidden);
  let mut vm = VmIdentity {
    cpu_brand: recipe.cpu.brand.as_deref().map(fill),
    hide_hypervisor: hidden,
    smbios: recipe.firmware.dmi.as_ref().map(|dmi| identity::smbios(dmi, run, &mut findings)),
    ..VmIdentity::default()
  };
  if let Some(bridge) = &recipe.devices.pci.host_bridge {
    match pci_id(bridge, None) {
      Ok(id) => vm.pci_host_bridge = Some(id),
      Err(err) => findings.push(Finding::warning(err)),
    }
  }
  if let Some(device) = recipe.devices.pci.extra.iter().find(|d| d.class == "isa-bridge") {
    match pci_id(&device.id, device.revision.as_deref()) {
      Ok(id) => vm.pci_isa_bridge = Some(id),
      Err(err) => findings.push(Finding::warning(err)),
    }
  }
  for device in &recipe.devices.pci.extra {
    if device.class != "isa-bridge" {
      findings.push(Finding::warning(format!("PCI device class {:?} is not supported; only isa-bridge", device.class)));
    }
  }

  let cpu_template = cpu::cpu_template(&recipe.cpu, hidden, host, &mut findings);

  let kernel_build_env = kernel::build_env(&recipe.os.kernel, &mut findings);

  let uptime_offset_secs = recipe.os.uptime.map_or(0, |d| d.as_secs() as i64);

  let sensors = recipe
    .devices
    .sensors
    .iter()
    .map(|s| SensorSpec {
      chip: s.chip.clone(),
      idle_mc: (s.idle_c * 1000.0).round() as i32,
      load_gain_mc: (s.load_gain_c * 1000.0).round() as i32,
      noise_mc: (s.noise_c * 1000.0).round() as i32,
      per_core: s.per_core,
    })
    .collect();

  let metadata = recipe.network.metadata.as_ref().map(|m| {
    let mut value = json!({});
    if let Some(file) = &m.file {
      let path = recipe.base_dir.join(file);
      let loaded = std::fs::read_to_string(&path)
        .map_err(|err| err.to_string())
        .and_then(|text| serde_yaml::from_str::<serde_json::Value>(&text).map_err(|err| err.to_string()));
      match loaded {
        Ok(loaded) => value = loaded,
        Err(err) => findings.push(Finding::warning(format!("cannot read metadata {}: {err}", path.display()))),
      }
    }
    if let (Some(provider), Some(object)) = (&m.provider, value.as_object_mut()) {
      object.entry("provider").or_insert_with(|| json!(provider));
    }
    value
  });

  // TODO(actor-network): the MAC is only checked against the vendor here;
  // actors have no NIC to apply it to (see `needy_core_actor::driver::linux_firecracker`).
  let network_mac = recipe.network.mac.as_ref().map(|t| fill(t.as_str()));
  cpu::check_cpu(&recipe.cpu, host, &mut findings);
  findings.extend(identity::check_vendor(recipe, network_mac.as_deref()));
  let cpu = CpuView {
    brand: vm.cpu_brand.clone(),
    signature: recipe.cpu.signature.as_ref().map(|s| (s.family, s.model, s.stepping)),
    hide: recipe.cpu.features.hide.clone(),
    hypervisor: recipe.cpu.hypervisor.map(|h| h == Hypervisor::Visible),
    threads_per_core: recipe.cpu.topology.as_ref().map_or(1, |t| t.threads_per_core.max(1)),
  };
  CompiledRecipe {
    vcpus,
    mem_mib,
    identity: vm,
    cpu,
    cpu_template,
    kernel_build_env,
    hostname: recipe.os.hostname.as_ref().map(|t| fill(t.as_str())),
    time: run.time.clone(),
    uptime_offset_secs,
    sensors,
    network_mac,
    interface: recipe.network.interface.clone(),
    metadata,
    os_image: recipe.os.image.clone(),
    required: recipe.required.clone(),
    findings,
  }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
