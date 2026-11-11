//! Recipes: one machine's CPU, memory, operating system, firmware, devices
//! and network identity. Compiling a recipe for a host is `needy-core`'s job.

mod cpu;
mod devices;
mod firmware;
mod kernel;
mod network;

use std::path::PathBuf;

use serde::Deserialize;

pub use cpu::{Cpu, Features, Hypervisor, Signature, Topology};
pub use devices::{Devices, Pci, PciDevice, Sensor};
pub use firmware::{Bios, ChassisType, Dmi, Firmware};
pub use kernel::{Kernel, KernelBuild, Os};
pub use network::{Metadata, Network};

use crate::value::ByteSize;
use crate::{Error, Kind, Source, merge, source};

/// One machine.
///
/// TODO(recipe): the raw `firecracker:` overrides in the recipe note are not
/// implemented, so `deny_unknown_fields` rejects them. Unlocks with an owner
/// decision on how they merge with the compiled recipe.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Recipe {
  pub cpu: Cpu,
  pub memory: Option<ByteSize>,
  /// The vCPU count; defaults to the topology's.
  pub vcpus: Option<u32>,
  pub os: Os,
  pub firmware: Firmware,
  pub devices: Devices,
  pub network: Network,
  /// The identity fields an actor must present; a runtime that cannot is
  /// an error in `profile check` and `run create`, where it is otherwise a
  /// warning.
  pub required: Vec<IdentityField>,
  /// The directory that relative paths in the recipe resolve against.
  #[serde(skip)]
  pub base_dir: PathBuf,
}

/// One part of a machine's identity, as a recipe requires it of the actor
/// runtime (see the container actor runtime design's Identity table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityField {
  Hostname,
  /// `/proc/cpuinfo`: brand, signature, flags, CPU count.
  Cpuinfo,
  /// The `CPUID` instruction itself.
  Cpuid,
  /// `/sys/class/dmi/id`.
  Dmi,
  /// PCI vendor and device IDs.
  Pci,
  /// hwmon sensors and the hwmon fault.
  Hwmon,
  /// `uname -r` and `/proc/version`.
  Kernel,
  /// The uptime offset.
  Uptime,
}

impl IdentityField {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Hostname => "hostname",
      Self::Cpuinfo => "cpuinfo",
      Self::Cpuid => "cpuid",
      Self::Dmi => "dmi",
      Self::Pci => "pci",
      Self::Hwmon => "hwmon",
      Self::Kernel => "kernel",
      Self::Uptime => "uptime",
    }
  }
}

/// Recipes declare no prepend lists; every list replaces the parent's.
const MERGE: merge::Rules = merge::Rules { prepend: &[] };

/// Reads a recipe and its `extends` chain.
pub fn load(source: &Source) -> Result<Recipe, Error> {
  let document = source::read_chain(source, Kind::Recipe, &MERGE)?;
  let mut recipe: Recipe = source::deserialize(document, Kind::Recipe, source)?;
  recipe.base_dir = source.base_dir();
  Ok(recipe)
}

/// Reads a recipe and its `extends` chain into one YAML document a client
/// sends, with `network.metadata.file` made absolute.
// TODO(runner-files): the metadata file stays a path on the client's
// machine; see `profile::resolve`.
pub fn resolve(source: &Source) -> Result<String, Error> {
  let mut document = source::read_chain(source, Kind::Recipe, &MERGE)?;
  if let Some(serde_yaml::Value::String(file)) =
    document.get_mut("network").and_then(|n| n.get_mut("metadata")).and_then(|m| m.get_mut("file"))
  {
    *file = source::absolute(file, &source.base_dir());
  }
  let _: Recipe = source::deserialize(document.clone(), Kind::Recipe, source)?;
  source::serialize(&document, Kind::Recipe, source)
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
