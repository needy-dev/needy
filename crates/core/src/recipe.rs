//! Recipes: one machine's image, kernel, CPU, memory, firmware, devices and
//! network identity, compiled into Firecracker and guest settings.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::json;

use needy_common::util::{mix, parse_duration};

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Signature {
  pub family: u32,
  pub model: u32,
  pub stepping: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Features {
  /// Linux `/proc/cpuinfo` flag names to clear.
  pub hide: Vec<String>,
  /// Flags the host must have.
  pub require: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
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

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct CpuSpec {
  pub brand: Option<String>,
  pub signature: Option<Signature>,
  /// `visible` or `hidden`.
  pub hypervisor: Option<String>,
  pub features: Features,
  pub topology: Option<Topology>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct KernelBuild {
  pub user: Option<String>,
  pub host: Option<String>,
  pub timestamp: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct KernelSpec {
  pub release: Option<String>,
  pub version: Option<String>,
  pub build: KernelBuild,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct OsSpec {
  pub image: Option<String>,
  pub kernel: KernelSpec,
  pub hostname: Option<String>,
  pub uptime: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Bios {
  pub vendor: Option<String>,
  pub version: Option<String>,
  pub date: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Dmi {
  pub sys_vendor: Option<String>,
  pub product_name: Option<String>,
  pub product_version: Option<String>,
  pub bios: Bios,
  pub board_vendor: Option<String>,
  pub board_name: Option<String>,
  pub chassis_type: Option<serde_yaml::Value>,
  pub serial: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct FirmwareSpec {
  pub dmi: Option<Dmi>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct PciDevice {
  pub id: String,
  pub class: String,
  pub revision: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct PciSpec {
  pub host_bridge: Option<String>,
  pub extra: Vec<PciDevice>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Sensor {
  pub chip: String,
  pub per_core: bool,
  pub idle_c: f64,
  pub load_gain_c: f64,
  pub noise_c: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct DevicesSpec {
  pub pci: PciSpec,
  pub sensors: Vec<Sensor>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct MetadataSpec {
  pub provider: Option<String>,
  pub file: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct NetworkSpec {
  pub interface: Option<String>,
  pub mac: Option<String>,
  pub metadata: Option<MetadataSpec>,
}

/// One machine.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Recipe {
  pub cpu: CpuSpec,
  /// `16GiB`, `512MiB`, or a number of MiB.
  pub memory: Option<serde_yaml::Value>,
  pub vcpus: Option<u32>,
  pub os: OsSpec,
  pub firmware: FirmwareSpec,
  pub devices: DevicesSpec,
  pub network: NetworkSpec,
  #[serde(skip)]
  pub base_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
  Error,
  Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
  pub severity: Severity,
  pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SensorSpec {
  pub chip: String,
  pub idle_mc: i32,
  pub load_gain_mc: i32,
  pub noise_mc: i32,
  pub per_core: bool,
}

/// A recipe compiled for one host and session seed.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledRecipe {
  pub vcpus: u32,
  pub mem_mib: u32,
  /// Variables read by the patched Firecracker.
  pub firecracker_env: BTreeMap<String, String>,
  /// Firecracker custom CPU template.
  pub cpu_template: serde_json::Value,
  /// Variables for building the guest kernel.
  pub kernel_build_env: BTreeMap<String, String>,
  pub hostname: Option<String>,
  pub uptime_offset_secs: i64,
  pub sensors: Vec<SensorSpec>,
  pub network_mac: Option<String>,
  pub interface: Option<String>,
  pub metadata: Option<serde_json::Value>,
  pub os_image: Option<String>,
  pub findings: Vec<Finding>,
}

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
  pub fn detect() -> Result<HostInfo> {
    if cfg!(target_os = "linux") {
      let text = std::fs::read_to_string("/proc/cpuinfo").context("cannot read /proc/cpuinfo")?;
      Ok(Self::from_cpuinfo(&text))
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

const BUILTINS: &[(&str, &str)] = &[
  ("gce-ubuntu-22.04", include_str!("builtin/gce-ubuntu-22.04.yaml")),
  ("dell-optiplex-ubuntu-24.04", include_str!("builtin/dell-optiplex-ubuntu-24.04.yaml")),
];

const MAX_EXTENDS: usize = 16;

impl Recipe {
  pub fn load(path: &Path) -> Result<Recipe> {
    let text = std::fs::read_to_string(path).with_context(|| format!("cannot read recipe {}", path.display()))?;
    let base = path.parent().unwrap_or(Path::new("."));
    Self::from_yaml(&text, base).with_context(|| format!("in recipe {}", path.display()))
  }

  pub fn from_yaml(text: &str, base_dir: &Path) -> Result<Recipe> {
    let value = resolve_value(text, base_dir, 0)?;
    let mut recipe: Recipe = serde_yaml::from_value(value).context("invalid recipe")?;
    recipe.base_dir = base_dir.to_path_buf();
    Ok(recipe)
  }

  pub fn compile(&self, host: &HostInfo, seed: u64) -> CompiledRecipe {
    let mut findings = Vec::new();
    let fill = |text: &str| fill_placeholders(text, seed);

    let topology = self.cpu.topology.clone();
    let vcpus = self.vcpus.or_else(|| topology.map(|t| t.cores.max(1) * t.threads_per_core.max(1))).unwrap_or(2);
    let mem_mib = match &self.memory {
      None => 1024,
      Some(value) => parse_memory(value).unwrap_or_else(|err| {
        findings.push(warning(format!("{err}; using 1024 MiB")));
        1024
      }),
    };

    let mut env = BTreeMap::new();
    if let Some(brand) = &self.cpu.brand {
      env.insert("NEEDY_BRAND_STRING".into(), fill(brand));
    }
    let hidden = self.cpu.hypervisor.as_deref() == Some("hidden");
    if hidden {
      env.insert("NEEDY_HIDE_HYPERVISOR".into(), "1".into());
    }
    if let Some(dmi) = &self.firmware.dmi {
      env.insert("NEEDY_SMBIOS".into(), smbios(dmi, seed, &mut findings));
    }
    if let Some(bridge) = &self.devices.pci.host_bridge {
      match pci_id(bridge, None) {
        Ok(id) => {
          env.insert("NEEDY_PCI_HOST_BRIDGE".into(), id);
        }
        Err(err) => findings.push(warning(err)),
      }
    }
    if let Some(device) = self.devices.pci.extra.iter().find(|d| d.class == "isa-bridge") {
      match pci_id(&device.id, device.revision.as_deref()) {
        Ok(id) => {
          env.insert("NEEDY_PCI_ISA_BRIDGE".into(), id);
        }
        Err(err) => findings.push(warning(err)),
      }
    }
    for device in &self.devices.pci.extra {
      if device.class != "isa-bridge" {
        findings.push(warning(format!("PCI device class {:?} is not supported; only isa-bridge", device.class)));
      }
    }

    let cpu_template = cpu_template(&self.cpu, hidden, &mut findings);

    let mut kernel_build_env = BTreeMap::new();
    let kernel = &self.os.kernel;
    for (key, value) in [
      ("NEEDY_KERNEL_RELEASE", &kernel.release),
      ("NEEDY_KERNEL_VERSION", &kernel.version),
      ("KBUILD_BUILD_USER", &kernel.build.user),
      ("KBUILD_BUILD_HOST", &kernel.build.host),
      ("KBUILD_BUILD_TIMESTAMP", &kernel.build.timestamp),
    ] {
      if let Some(value) = value {
        kernel_build_env.insert(key.to_string(), value.clone());
      }
    }

    let uptime_offset_secs = match &self.os.uptime {
      None => 0,
      Some(text) => match parse_duration(text) {
        Ok(d) => d.as_secs() as i64,
        Err(err) => {
          findings.push(warning(format!("os.uptime: {err}")));
          0
        }
      },
    };

    let sensors = self
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

    let metadata = self.network.metadata.as_ref().map(|m| {
      let mut value = json!({});
      if let Some(file) = &m.file {
        let path = self.base_dir.join(file);
        match std::fs::read_to_string(&path)
          .map_err(anyhow::Error::from)
          .and_then(|t| serde_yaml::from_str::<serde_json::Value>(&t).map_err(Into::into))
        {
          Ok(loaded) => value = loaded,
          Err(err) => findings.push(warning(format!("cannot read metadata {}: {err}", path.display()))),
        }
      }
      if let (Some(provider), Some(object)) = (&m.provider, value.as_object_mut()) {
        object.entry("provider").or_insert_with(|| json!(provider));
      }
      value
    });

    let mut compiled = CompiledRecipe {
      vcpus,
      mem_mib,
      firecracker_env: env,
      cpu_template,
      kernel_build_env,
      hostname: self.os.hostname.as_deref().map(fill),
      uptime_offset_secs,
      sensors,
      network_mac: self.network.mac.as_deref().map(fill),
      interface: self.network.interface.clone(),
      metadata,
      os_image: self.os.image.clone(),
      findings,
    };
    check_cpu(&self.cpu, host, &mut compiled.findings);
    let vendor = check_vendor(self, &compiled);
    compiled.findings.extend(vendor);
    compiled
  }
}

fn warning(message: impl Into<String>) -> Finding {
  Finding {
    severity: Severity::Warning,
    message: message.into(),
  }
}

fn error(message: impl Into<String>) -> Finding {
  Finding {
    severity: Severity::Error,
    message: message.into(),
  }
}

fn resolve_value(text: &str, base_dir: &Path, depth: usize) -> Result<serde_yaml::Value> {
  if depth > MAX_EXTENDS {
    bail!("recipe extends nests more than {MAX_EXTENDS} levels");
  }
  let mut value: serde_yaml::Value = if text.trim().is_empty() {
    serde_yaml::Value::Mapping(Default::default())
  } else {
    serde_yaml::from_str(text).context("invalid recipe YAML")?
  };
  let parent = value.as_mapping_mut().and_then(|m| m.remove("extends")).and_then(|v| v.as_str().map(str::to_string));
  let Some(parent) = parent else {
    return Ok(value);
  };
  let parent_value = if let Some(name) = parent.strip_prefix("builtin:") {
    let (_, text) = BUILTINS.iter().find(|(n, _)| *n == name).with_context(|| format!("unknown built-in recipe {name:?}"))?;
    resolve_value(text, base_dir, depth + 1)?
  } else {
    let path = base_dir.join(&parent);
    let text = std::fs::read_to_string(&path).with_context(|| format!("cannot read parent recipe {}", path.display()))?;
    let parent_base = path.parent().unwrap_or(base_dir).to_path_buf();
    resolve_value(&text, &parent_base, depth + 1).with_context(|| format!("in parent recipe {}", path.display()))?
  };
  Ok(merge(parent_value, value))
}

/// Deep merge: mappings merge key by key, anything else is replaced.
fn merge(base: serde_yaml::Value, over: serde_yaml::Value) -> serde_yaml::Value {
  match (base, over) {
    (serde_yaml::Value::Mapping(mut base), serde_yaml::Value::Mapping(over)) => {
      for (key, value) in over {
        let merged = match base.remove(&key) {
          Some(old) => merge(old, value),
          None => value,
        };
        base.insert(key, merged);
      }
      serde_yaml::Value::Mapping(base)
    }
    (_, over) => over,
  }
}

fn parse_memory(value: &serde_yaml::Value) -> Result<u32> {
  if let Some(n) = value.as_u64() {
    return u32::try_from(n).context("memory too large");
  }
  let text = value.as_str().context("memory must be a size")?.trim();
  let split = text.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(text.len());
  let (number, unit) = text.split_at(split);
  let number: f64 = number.parse().with_context(|| format!("invalid memory size {text:?}"))?;
  let mib = match unit.trim() {
    "" | "M" | "MiB" | "MB" => number,
    "G" | "GiB" | "GB" => number * 1024.0,
    "T" | "TiB" | "TB" => number * 1024.0 * 1024.0,
    other => bail!("invalid memory unit {other:?}"),
  };
  Ok(mib.round() as u32)
}

/// `{hash}` becomes 8 hex digits and `{seq}` 2 hex digits derived from the seed.
fn fill_placeholders(text: &str, seed: u64) -> String {
  text.replace("{hash}", &format!("{:08x}", mix(seed, 1) as u32)).replace("{seq}", &format!("{:02x}", mix(seed, 2) as u8))
}

fn session_uuid(seed: u64) -> String {
  let a = mix(seed, 10);
  let b = mix(seed, 11);
  let mut bytes = [0u8; 16];
  bytes[..8].copy_from_slice(&a.to_be_bytes());
  bytes[8..].copy_from_slice(&b.to_be_bytes());
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
  format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

fn chassis_number(value: &serde_yaml::Value) -> Option<u32> {
  if let Some(n) = value.as_u64() {
    return u32::try_from(n).ok();
  }
  Some(match value.as_str()? {
    "other" => 1,
    "unknown" => 2,
    "desktop" => 3,
    "low-profile-desktop" => 4,
    "tower" => 7,
    "portable" => 8,
    "laptop" => 9,
    "notebook" => 10,
    "all-in-one" => 13,
    "rack" => 23,
    "mini-pc" => 35,
    _ => return None,
  })
}

/// `NEEDY_SMBIOS` for the patched Firecracker: `key=value;…`.
fn smbios(dmi: &Dmi, seed: u64, findings: &mut Vec<Finding>) -> String {
  let fill = |text: &str| fill_placeholders(text, seed).replace(';', ",");
  let mut fields = BTreeMap::new();
  let mut put = |key: &str, value: Option<&String>| {
    if let Some(value) = value {
      fields.insert(key.to_string(), fill(value));
    }
  };
  put("sys_vendor", dmi.sys_vendor.as_ref());
  put("product_name", dmi.product_name.as_ref());
  put("product_version", dmi.product_version.as_ref());
  put("serial", dmi.serial.as_ref());
  put("board_vendor", dmi.board_vendor.as_ref().or(dmi.sys_vendor.as_ref()));
  put("board_name", dmi.board_name.as_ref().or(dmi.product_name.as_ref()));
  put("bios_vendor", dmi.bios.vendor.as_ref());
  put("bios_version", dmi.bios.version.as_ref());
  put("bios_date", dmi.bios.date.as_ref());
  fields.insert("uuid".into(), session_uuid(seed));
  if let Some(value) = &dmi.chassis_type {
    match chassis_number(value) {
      Some(n) => {
        fields.insert("chassis_type".into(), n.to_string());
      }
      None => findings.push(warning(format!("unknown chassis_type {value:?}"))),
    }
  }
  fields.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(";")
}

/// Normalizes `vvvv:dddd[:rr]` to `vvvv:dddd:rr`.
fn pci_id(text: &str, revision: Option<&str>) -> Result<String, String> {
  let parts: Vec<&str> = text.split(':').collect();
  let hex4 = |s: &str| s.len() == 4 && s.chars().all(|c| c.is_ascii_hexdigit());
  let hex2 = |s: &str| s.len() == 2 && s.chars().all(|c| c.is_ascii_hexdigit());
  let (vendor, device, rev) = match parts.as_slice() {
    [v, d] => (*v, *d, revision.unwrap_or("00")),
    [v, d, r] => (*v, *d, *r),
    _ => return Err(format!("invalid PCI id {text:?}")),
  };
  if !hex4(vendor) || !hex4(device) || !hex2(rev) {
    return Err(format!("invalid PCI id {text:?}"));
  }
  Ok(format!("{}:{}:{}", vendor.to_ascii_lowercase(), device.to_ascii_lowercase(), rev.to_ascii_lowercase()))
}

/// CPUID leaf 1 EAX for a family, model and stepping.
pub(crate) fn encode_signature(sig: &Signature) -> u32 {
  let family_field = sig.family.min(15);
  let extended_family = sig.family.saturating_sub(15);
  let extended_model = if sig.family == 6 || sig.family >= 15 {
    (sig.model >> 4) & 0xf
  } else {
    0
  };
  (extended_family << 20) | (extended_model << 16) | (family_field << 8) | ((sig.model & 0xf) << 4) | (sig.stepping & 0xf)
}

/// Linux `/proc/cpuinfo` name → (leaf, subleaf, register, bit).
const FEATURES: &[(&str, u32, u32, &str, u32)] = &[
  ("sse3", 1, 0, "ecx", 0),
  ("pclmulqdq", 1, 0, "ecx", 1),
  ("ssse3", 1, 0, "ecx", 9),
  ("fma", 1, 0, "ecx", 12),
  ("cx16", 1, 0, "ecx", 13),
  ("sse4_1", 1, 0, "ecx", 19),
  ("sse4_2", 1, 0, "ecx", 20),
  ("movbe", 1, 0, "ecx", 22),
  ("popcnt", 1, 0, "ecx", 23),
  ("aes", 1, 0, "ecx", 25),
  ("xsave", 1, 0, "ecx", 26),
  ("avx", 1, 0, "ecx", 28),
  ("f16c", 1, 0, "ecx", 29),
  ("rdrand", 1, 0, "ecx", 30),
  ("hypervisor", 1, 0, "ecx", 31),
  ("fsgsbase", 7, 0, "ebx", 0),
  ("bmi1", 7, 0, "ebx", 3),
  ("hle", 7, 0, "ebx", 4),
  ("avx2", 7, 0, "ebx", 5),
  ("smep", 7, 0, "ebx", 7),
  ("bmi2", 7, 0, "ebx", 8),
  ("erms", 7, 0, "ebx", 9),
  ("invpcid", 7, 0, "ebx", 10),
  ("rtm", 7, 0, "ebx", 11),
  ("mpx", 7, 0, "ebx", 14),
  ("avx512f", 7, 0, "ebx", 16),
  ("avx512dq", 7, 0, "ebx", 17),
  ("rdseed", 7, 0, "ebx", 18),
  ("adx", 7, 0, "ebx", 19),
  ("smap", 7, 0, "ebx", 20),
  ("avx512ifma", 7, 0, "ebx", 21),
  ("clflushopt", 7, 0, "ebx", 23),
  ("clwb", 7, 0, "ebx", 24),
  ("avx512pf", 7, 0, "ebx", 26),
  ("avx512er", 7, 0, "ebx", 27),
  ("avx512cd", 7, 0, "ebx", 28),
  ("sha_ni", 7, 0, "ebx", 29),
  ("avx512bw", 7, 0, "ebx", 30),
  ("avx512vl", 7, 0, "ebx", 31),
  ("avx512vbmi", 7, 0, "ecx", 1),
  ("umip", 7, 0, "ecx", 2),
  ("pku", 7, 0, "ecx", 3),
  ("ospke", 7, 0, "ecx", 4),
  ("waitpkg", 7, 0, "ecx", 5),
  ("avx512_vbmi2", 7, 0, "ecx", 6),
  ("gfni", 7, 0, "ecx", 8),
  ("vaes", 7, 0, "ecx", 9),
  ("vpclmulqdq", 7, 0, "ecx", 10),
  ("avx512_vnni", 7, 0, "ecx", 11),
  ("avx512_bitalg", 7, 0, "ecx", 12),
  ("avx512_vpopcntdq", 7, 0, "ecx", 14),
  ("la57", 7, 0, "ecx", 16),
  ("rdpid", 7, 0, "ecx", 22),
  ("movdiri", 7, 0, "ecx", 27),
  ("movdir64b", 7, 0, "ecx", 28),
  ("avx512_vp2intersect", 7, 0, "edx", 8),
  ("md_clear", 7, 0, "edx", 10),
  ("serialize", 7, 0, "edx", 14),
  ("hybrid_cpu", 7, 0, "edx", 15),
  ("tsxldtrk", 7, 0, "edx", 16),
  ("pconfig", 7, 0, "edx", 18),
  ("amx_bf16", 7, 0, "edx", 22),
  ("avx512_fp16", 7, 0, "edx", 23),
  ("amx_tile", 7, 0, "edx", 24),
  ("amx_int8", 7, 0, "edx", 25),
  ("avx_vnni", 7, 1, "eax", 4),
  ("avx512_bf16", 7, 1, "eax", 5),
];

pub(crate) fn feature_bit(name: &str) -> Option<(u32, u32, &'static str, u32)> {
  FEATURES.iter().find(|(n, ..)| *n == name).map(|&(_, leaf, subleaf, reg, bit)| (leaf, subleaf, reg, bit))
}

fn cpu_template(cpu: &CpuSpec, hidden: bool, findings: &mut Vec<Finding>) -> serde_json::Value {
  // (leaf, subleaf, register) → 32 bitmap characters, index 0 is bit 31.
  let mut registers: BTreeMap<(u32, u32, &str), [char; 32]> = BTreeMap::new();
  let mut set_bit = |leaf: u32, subleaf: u32, reg: &'static str, bit: u32, value: char| {
    let bits = registers.entry((leaf, subleaf, reg)).or_insert(['x'; 32]);
    bits[31 - bit as usize] = value;
  };
  if let Some(sig) = &cpu.signature {
    let eax = encode_signature(sig);
    for bit in 0..32 {
      set_bit(1, 0, "eax", bit, if eax >> bit & 1 == 1 { '1' } else { '0' });
    }
  }
  if hidden {
    set_bit(1, 0, "ecx", 31, '0');
    for reg in ["ebx", "ecx", "edx"] {
      for bit in 0..32 {
        set_bit(0x4000_0000, 0, reg, bit, '0');
      }
    }
  }
  for name in &cpu.features.hide {
    match feature_bit(name) {
      Some((leaf, subleaf, reg, bit)) => set_bit(leaf, subleaf, reg, bit, '0'),
      None => findings.push(warning(format!("cpu.features.hide: unknown feature {name:?}"))),
    }
  }
  let mut leaves: BTreeMap<(u32, u32), Vec<serde_json::Value>> = BTreeMap::new();
  for ((leaf, subleaf, reg), bits) in registers {
    let bitmap: String = std::iter::once("0b".to_string()).chain(bits.iter().map(char::to_string)).collect();
    leaves.entry((leaf, subleaf)).or_default().push(json!({"register": reg, "bitmap": bitmap}));
  }
  let modifiers: Vec<serde_json::Value> = leaves
    .into_iter()
    .map(|((leaf, subleaf), modifiers)| {
      json!({
          "leaf": format!("{leaf:#x}"),
          "subleaf": format!("{subleaf:#x}"),
          "flags": 0,
          "modifiers": modifiers,
      })
    })
    .collect();
  json!({ "cpuid_modifiers": modifiers })
}

struct ModelProfile {
  family: u32,
  models: &'static [u32],
  name: &'static str,
  requires: &'static [&'static str],
  newer: &'static [&'static str],
}

const MODELS: &[ModelProfile] = &[
  ModelProfile {
    family: 6,
    models: &[79],
    name: "Broadwell-EP",
    requires: &["avx2", "bmi2", "adx"],
    newer: &[
      "avx512f",
      "clwb",
      "sha_ni",
      "avx_vnni",
      "serialize",
      "hybrid_cpu",
      "gfni",
      "vaes",
      "vpclmulqdq",
      "umip",
      "pku",
      "movdiri",
      "movdir64b",
      "waitpkg",
    ],
  },
  ModelProfile {
    family: 6,
    models: &[85],
    name: "Skylake-SP/Cascade Lake",
    requires: &["avx512f", "avx512dq", "avx512cd", "avx512bw", "avx512vl"],
    newer: &[
      "avx_vnni",
      "serialize",
      "hybrid_cpu",
      "amx_tile",
      "amx_int8",
      "amx_bf16",
      "gfni",
      "vaes",
      "vpclmulqdq",
      "movdiri",
      "movdir64b",
      "waitpkg",
      "avx512_fp16",
      "sha_ni",
    ],
  },
  ModelProfile {
    family: 6,
    models: &[106, 108],
    name: "Ice Lake-SP",
    requires: &["avx512f", "gfni", "vaes", "vpclmulqdq", "sha_ni"],
    newer: &[
      "avx_vnni",
      "serialize",
      "hybrid_cpu",
      "amx_tile",
      "amx_int8",
      "amx_bf16",
      "avx512_fp16",
    ],
  },
  ModelProfile {
    family: 6,
    models: &[143],
    name: "Sapphire Rapids",
    requires: &[
      "avx512f",
      "amx_tile",
      "amx_int8",
      "serialize",
      "avx512_fp16",
    ],
    newer: &["hybrid_cpu"],
  },
  ModelProfile {
    family: 6,
    models: &[151, 154],
    name: "Alder Lake",
    requires: &["avx_vnni", "serialize"],
    newer: &["avx512f", "amx_tile"],
  },
  ModelProfile {
    family: 6,
    models: &[183, 186, 191],
    name: "Raptor Lake",
    requires: &["avx_vnni", "serialize"],
    newer: &["avx512f", "amx_tile"],
  },
];

fn check_cpu(cpu: &CpuSpec, host: &HostInfo, findings: &mut Vec<Finding>) {
  if host.flags.is_empty() {
    return;
  }
  let model = cpu.signature.as_ref().and_then(|sig| MODELS.iter().find(|m| m.family == sig.family && m.models.contains(&sig.model)));
  let mut required: Vec<(&str, String)> = cpu.features.require.iter().map(|f| (f.as_str(), "cpu.features.require".to_string())).collect();
  if let Some(model) = model {
    required.extend(model.requires.iter().map(|f| (*f, model.name.to_string())));
  }
  for (feature, source) in required {
    if !host.flags.contains(feature) {
      findings.push(error(format!("{source} needs {feature}, but the host CPU ({}) lacks it", host.model_name)));
    }
  }
  if let Some(model) = model {
    for feature in model.newer {
      if host.flags.contains(*feature) && !cpu.features.hide.iter().any(|h| h == feature) {
        findings.push(warning(format!("the host exposes {feature}, which {} does not have; add it to cpu.features.hide", model.name)));
      }
    }
  }
}

fn vendor_of_dmi(vendor: &str) -> &'static str {
  let v = vendor.to_ascii_lowercase();
  if v.contains("google") {
    "gce"
  } else if v.contains("amazon") {
    "aws"
  } else if v.contains("microsoft") {
    "azure"
  } else {
    "other"
  }
}

fn vendor_of_mac(mac: &str) -> Option<&'static str> {
  let m = mac.to_ascii_lowercase();
  if m.starts_with("42:01:") {
    Some("gce")
  } else if m.starts_with("0e:") {
    Some("aws")
  } else if ["00:0d:3a:", "00:22:48:", "60:45:bd:"].iter().any(|p| m.starts_with(p)) {
    Some("azure")
  } else {
    None
  }
}

fn vendor_of_kernel(release: &str) -> Option<&'static str> {
  if release.ends_with("-gcp") {
    Some("gce")
  } else if release.ends_with("-aws") {
    Some("aws")
  } else if release.ends_with("-azure") {
    Some("azure")
  } else if release.ends_with("-generic") {
    Some("other")
  } else {
    None
  }
}

fn vendor_of_provider(provider: &str) -> &'static str {
  match provider.to_ascii_lowercase().as_str() {
    "gce" | "gcp" | "google" => "gce",
    "aws" | "ec2" | "amazon" => "aws",
    "azure" | "microsoft" => "azure",
    _ => "other",
  }
}

/// Warns when DMI vendor, MAC prefix, metadata provider and kernel flavour
/// name different clouds or machines.
fn check_vendor(recipe: &Recipe, compiled: &CompiledRecipe) -> Vec<Finding> {
  let mut sources: Vec<(&str, &'static str)> = Vec::new();
  if let Some(vendor) = recipe.firmware.dmi.as_ref().and_then(|d| d.sys_vendor.as_deref()) {
    sources.push(("DMI vendor", vendor_of_dmi(vendor)));
  }
  if let Some(v) = compiled.network_mac.as_deref().and_then(vendor_of_mac) {
    sources.push(("MAC prefix", v));
  }
  if let Some(provider) = recipe.network.metadata.as_ref().and_then(|m| m.provider.as_deref()) {
    sources.push(("metadata provider", vendor_of_provider(provider)));
  }
  if let Some(v) = recipe.os.kernel.release.as_deref().and_then(vendor_of_kernel) {
    sources.push(("kernel release", v));
  }
  let distinct: BTreeSet<&str> = sources.iter().map(|(_, v)| *v).collect();
  if distinct.len() > 1 {
    let detail = sources.iter().map(|(what, v)| format!("{what}={v}")).collect::<Vec<_>>().join(", ");
    vec![warning(format!("machine identity disagrees: {detail}"))]
  } else {
    Vec::new()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const RAPTOR_LAKE_FLAGS: &str = "fpu vme sse sse2 ssse3 fma sse4_1 sse4_2 avx avx2 bmi1 bmi2 adx sha_ni gfni vaes vpclmulqdq avx_vnni serialize hybrid_cpu movdiri movdir64b waitpkg umip pku ospke rdseed clflushopt clwb";

  fn raptor_lake() -> HostInfo {
    HostInfo::from_cpuinfo(&format!(
      "processor\t: 0\nvendor_id\t: GenuineIntel\nmodel name\t: 13th Gen Intel(R) Core(TM) i5-13600KF\nflags\t\t: {RAPTOR_LAKE_FLAGS}\n"
    ))
  }

  const GCE: &str = r#"
extends: builtin:gce-ubuntu-22.04
cpu:
  brand: "Intel(R) Xeon(R) CPU @ 2.20GHz"
  signature: { family: 6, model: 85, stepping: 7 }
  hypervisor: hidden
  features: { hide: [avx_vnni, serialize] }
  topology: { cores: 2, threads_per_core: 2 }
memory: 16GiB
os:
  hostname: "instance-{hash}"
  uptime: 3d4h
"#;

  #[test]
  fn signature_encoding() {
    let sig = |family, model, stepping| Signature {
      family,
      model,
      stepping,
    };
    assert_eq!(encode_signature(&sig(6, 85, 7)), 0x0005_0657);
    assert_eq!(encode_signature(&sig(6, 183, 1)), 0x000b_0671);
    assert_eq!(encode_signature(&sig(25, 1, 1)), 0x00a0_0f11);
  }

  #[test]
  fn compiles_gce_recipe() {
    let recipe = Recipe::from_yaml(GCE, Path::new(".")).unwrap();
    let c = recipe.compile(&HostInfo::default(), 7);
    assert_eq!(c.vcpus, 4);
    assert_eq!(c.mem_mib, 16384);
    assert_eq!(c.firecracker_env["NEEDY_BRAND_STRING"], "Intel(R) Xeon(R) CPU @ 2.20GHz");
    assert_eq!(c.firecracker_env["NEEDY_HIDE_HYPERVISOR"], "1");
    assert_eq!(c.firecracker_env["NEEDY_PCI_HOST_BRIDGE"], "8086:1237:02");
    assert_eq!(c.firecracker_env["NEEDY_PCI_ISA_BRIDGE"], "8086:7110:03");
    let smbios = &c.firecracker_env["NEEDY_SMBIOS"];
    assert!(smbios.contains("sys_vendor=Google;"), "{smbios}");
    assert!(smbios.contains("product_name=Google Compute Engine"), "{smbios}");
    assert!(smbios.contains("chassis_type=1"), "{smbios}");
    assert!(smbios.contains("bios_date=01/01/2011"), "{smbios}");
    assert!(smbios.contains("uuid="), "{smbios}");
    assert_eq!(c.uptime_offset_secs, 3 * 86400 + 4 * 3600);
    let hostname = c.hostname.clone().unwrap();
    assert!(hostname.starts_with("instance-") && hostname.len() == "instance-".len() + 8);
    assert!(c.network_mac.as_deref().unwrap().starts_with("42:01:0a:80:00:"));
    assert_eq!(c.interface.as_deref(), Some("ens4"));
    assert_eq!(c.metadata.as_ref().unwrap()["provider"], "gce");
    assert_eq!(c.os_image.as_deref(), Some("ubuntu:22.04"));
    assert_eq!(c.kernel_build_env["NEEDY_KERNEL_RELEASE"], "6.5.0-1020-gcp");
    assert_eq!(c.kernel_build_env["KBUILD_BUILD_USER"], "buildd");

    let modifiers = c.cpu_template["cpuid_modifiers"].as_array().unwrap();
    let leaf =
      |l: &str, s: &str| modifiers.iter().find(|m| m["leaf"] == l && m["subleaf"] == s).unwrap()["modifiers"].as_array().unwrap().clone();
    let reg = |mods: &[serde_json::Value], r: &str| -> String {
      mods.iter().find(|m| m["register"] == r).unwrap()["bitmap"].as_str().unwrap().to_string()
    };
    let leaf1 = leaf("0x1", "0x0");
    assert_eq!(reg(&leaf1, "eax"), format!("0b{:032b}", 0x0005_0657));
    assert_eq!(reg(&leaf1, "ecx"), format!("0b0{}", "x".repeat(31)));
    let leaf7 = leaf("0x7", "0x0");
    let edx = reg(&leaf7, "edx");
    assert_eq!(edx.len(), 34);
    assert_eq!(&edx[2 + 31 - 14..2 + 31 - 13], "0", "serialize is bit 14");
    let leaf71 = leaf("0x7", "0x1");
    let eax = reg(&leaf71, "eax");
    assert_eq!(&eax[2 + 31 - 4..2 + 31 - 3], "0", "avx_vnni is bit 4");
    let kvm = leaf("0x40000000", "0x0");
    assert_eq!(reg(&kvm, "ebx"), format!("0b{}", "0".repeat(32)));

    // Same seed, same values; different seed, different values.
    let again = recipe.compile(&HostInfo::default(), 7);
    assert_eq!(again.hostname, c.hostname);
    assert_eq!(again.firecracker_env, c.firecracker_env);
    let other = recipe.compile(&HostInfo::default(), 8);
    assert_ne!(other.hostname, c.hostname);
    assert!(c.findings.is_empty(), "{:?}", c.findings);
  }

  #[test]
  fn cascade_lake_on_raptor_lake() {
    let recipe =
      Recipe::from_yaml("cpu: { signature: { family: 6, model: 85, stepping: 7 }, features: { hide: [serialize] } }", Path::new("."))
        .unwrap();
    let c = recipe.compile(&raptor_lake(), 1);
    let errors: Vec<_> = c.findings.iter().filter(|f| f.severity == Severity::Error).collect();
    assert!(errors.iter().any(|f| f.message.contains("avx512f")), "{errors:?}");
    let warnings: Vec<_> = c.findings.iter().filter(|f| f.severity == Severity::Warning).map(|f| f.message.clone()).collect();
    assert!(warnings.iter().any(|w| w.contains("avx_vnni")), "{warnings:?}");
    assert!(warnings.iter().any(|w| w.contains("hybrid_cpu")), "{warnings:?}");
    assert!(!warnings.iter().any(|w| w.contains("serialize")), "hidden features do not warn");
  }

  #[test]
  fn raptor_lake_identity_on_raptor_lake_is_clean() {
    let recipe =
      Recipe::from_yaml("cpu: { signature: { family: 6, model: 183, stepping: 1 }, features: { require: [avx2] } }", Path::new("."))
        .unwrap();
    assert!(recipe.compile(&raptor_lake(), 1).findings.is_empty());
  }

  #[test]
  fn vendor_disagreement_warns() {
    let recipe = Recipe::from_yaml(
      "extends: builtin:gce-ubuntu-22.04\nnetwork: { mac: \"0e:11:22:33:44:55\" }\nos: { kernel: { release: 6.8.0-1-aws } }",
      Path::new("."),
    )
    .unwrap();
    let c = recipe.compile(&HostInfo::default(), 1);
    assert!(c.findings.iter().any(|f| f.message.contains("machine identity disagrees")), "{:?}", c.findings);
  }

  #[test]
  fn builtins_compile() {
    for (name, _) in BUILTINS {
      let r = Recipe::from_yaml(&format!("extends: builtin:{name}"), Path::new(".")).unwrap();
      let c = r.compile(&HostInfo::default(), 3);
      assert!(c.findings.is_empty(), "{name}: {:?}", c.findings);
    }
    let dell = Recipe::from_yaml("extends: builtin:dell-optiplex-ubuntu-24.04", Path::new(".")).unwrap().compile(&HostInfo::default(), 3);
    assert_eq!(dell.sensors.len(), 3);
    assert_eq!(dell.sensors[0].idle_mc, 34000);
    assert!(dell.firecracker_env["NEEDY_SMBIOS"].contains("chassis_type=7"));
    assert!(Recipe::from_yaml("extends: builtin:nope", Path::new(".")).is_err());
  }

  #[test]
  fn memory_and_findings() {
    assert_eq!(parse_memory(&serde_yaml::Value::from(512)).unwrap(), 512);
    assert_eq!(parse_memory(&serde_yaml::Value::from("2G")).unwrap(), 2048);
    assert_eq!(parse_memory(&serde_yaml::Value::from("512MiB")).unwrap(), 512);
    assert!(parse_memory(&serde_yaml::Value::from("2Q")).is_err());
    let r = Recipe::from_yaml("cpu: { features: { hide: [not_a_flag] } }\ndevices: { pci: { host_bridge: zz } }", Path::new(".")).unwrap();
    let c = r.compile(&HostInfo::default(), 0);
    assert_eq!(c.findings.len(), 2, "{:?}", c.findings);
  }

  #[test]
  fn host_info_parsing() {
    let host = raptor_lake();
    assert!(host.flags.contains("avx_vnni"));
    assert_eq!(host.vendor, "GenuineIntel");
    assert!(host.model_name.contains("i5-13600KF"));
    assert!(feature_bit("avx512f").is_some());
    assert!(feature_bit("bogus").is_none());
  }
}
