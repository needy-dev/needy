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
  /// The directory that relative paths in the recipe resolve against.
  #[serde(skip)]
  pub base_dir: PathBuf,
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

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
