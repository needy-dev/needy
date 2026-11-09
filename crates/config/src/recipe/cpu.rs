//! The CPU a recipe presents: brand, signature, hypervisor bit, features and
//! topology.

use serde::Deserialize;

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Cpu {
  pub brand: Option<String>,
  pub signature: Option<Signature>,
  pub hypervisor: Option<Hypervisor>,
  pub features: Features,
  pub topology: Option<Topology>,
}

/// CPUID leaf 1 family, model and stepping.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Signature {
  pub family: u32,
  pub model: u32,
  pub stepping: u32,
}

/// Whether the guest sees the hypervisor bit and the KVM signature leaf.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Hypervisor {
  Visible,
  Hidden,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Features {
  /// Linux `/proc/cpuinfo` flag names to clear; hide only, never add.
  pub hide: Vec<String>,
  /// Flags the host must have.
  pub require: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Topology {
  pub cores: u32,
  pub threads_per_core: u32,
}

impl Default for Topology {
  fn default() -> Self {
    Self {
      cores: 1,
      threads_per_core: 1,
    }
  }
}
