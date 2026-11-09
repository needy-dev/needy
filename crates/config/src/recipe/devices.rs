//! Devices a recipe presents: PCI identity and hardware sensors.

use serde::Deserialize;

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Devices {
  pub pci: Pci,
  pub sensors: Vec<Sensor>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Pci {
  /// `vvvv:dddd[:rr]`.
  pub host_bridge: Option<String>,
  pub extra: Vec<PciDevice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PciDevice {
  /// `vvvv:dddd[:rr]`.
  pub id: String,
  /// Only `isa-bridge` is supported.
  pub class: String,
  #[serde(default)]
  pub revision: Option<String>,
}

/// A fake hwmon chip whose readings follow load.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sensor {
  /// `coretemp`, `nvme` or `acpitz`.
  pub chip: String,
  #[serde(default)]
  pub per_core: bool,
  #[serde(default)]
  pub idle_c: f64,
  #[serde(default)]
  pub load_gain_c: f64,
  #[serde(default)]
  pub noise_c: f64,
}
