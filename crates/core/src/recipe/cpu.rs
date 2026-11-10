//! CPU identity: CPUID signature encoding, the Linux feature table, the
//! Firecracker CPU template, and checks of a CPU model against the host.

use std::collections::BTreeMap;

use needy_config::recipe::{Cpu, Signature};
use needy_config::validate::Finding;
use serde_json::json;

use super::host::HostInfo;

/// CPUID leaf 1 EAX for a family, model and stepping.
pub(super) fn encode_signature(sig: &Signature) -> u32 {
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
  // Source: Intel SDM Vol. 2A, order 253666-088US (June 2025), Table 3-17,
  // "Structured Extended Feature Enumeration Sub-leaf (Initial EAX Value = 07H,
  // ECX = 1)": EAX bit 04 AVX-VNNI, bit 05 AVX512-BF16,
  // https://cdrdv2-public.intel.com/858446/253666-088-sdm-vol-2a.pdf
  // Reference: https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html
  // NOTICE: the same table says a subleaf above the one leaf 07H, subleaf 0
  // reports in EAX returns zeros. CPUs without subleaf 1, such as Cascade
  // Lake, have no such entry in KVM's supported CPUID, so `cpu_template`
  // leaves these two out there.
  ("avx_vnni", 7, 1, "eax", 4),
  ("avx512_bf16", 7, 1, "eax", 5),
];

pub(super) fn feature_bit(name: &str) -> Option<(u32, u32, &'static str, u32)> {
  FEATURES.iter().find(|(n, ..)| *n == name).map(|&(_, leaf, subleaf, reg, bit)| (leaf, subleaf, reg, bit))
}

/// The Firecracker custom CPU template for `cpu`. Hiding a feature only
/// clears bits in CPUID entries `host`'s KVM supports: Firecracker refuses a
/// template that changes any other entry, and the guest cannot see a feature
/// in an entry it does not have.
// Source: https://github.com/firecracker-microvm/firecracker/blob/95f868c8e345b1cc8faccd1a3c910b4989dc3f58/src/vmm/src/cpu_config/x86_64/mod.rs#L43-L75
// (v1.17.0: `apply_template_to_cpuid` looks each modifier's leaf and subleaf
// up in the CPUID built from `KVM_GET_SUPPORTED_CPUID`,
// https://github.com/firecracker-microvm/firecracker/blob/95f868c8e345b1cc8faccd1a3c910b4989dc3f58/src/vmm/src/arch/x86_64/mod.rs#L205-L207,
// and fails with "Template changes a CPUID entry not supported by KVM").
// Reference: https://www.kernel.org/doc/html/v6.12/virt/kvm/api.html#kvm-get-supported-cpuid
pub(super) fn cpu_template(cpu: &Cpu, hidden: bool, host: &HostInfo, findings: &mut Vec<Finding>) -> serde_json::Value {
  // NOTICE: with no KVM to ask (`kvm_cpuid` is `None`), every hide is kept;
  // a host without KVM cannot start the actor anyway.
  let supported = |leaf: u32, subleaf: u32| host.kvm_cpuid.as_ref().is_none_or(|entries| entries.contains(&(leaf, subleaf)));
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
    if supported(0x4000_0000, 0) {
      for reg in ["ebx", "ecx", "edx"] {
        for bit in 0..32 {
          set_bit(0x4000_0000, 0, reg, bit, '0');
        }
      }
    }
  }
  for name in &cpu.features.hide {
    match feature_bit(name) {
      Some((leaf, subleaf, reg, bit)) if supported(leaf, subleaf) => set_bit(leaf, subleaf, reg, bit, '0'),
      Some((leaf, subleaf, ..)) => findings.push(Finding::warning(format!(
        "cpu.features.hide: {name} has no effect: this host's KVM has no CPUID leaf {leaf:#x}, subleaf {subleaf:#x}, so the guest cannot see it"
      ))),
      None => findings.push(Finding::warning(format!("cpu.features.hide: unknown feature {name:?}"))),
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

pub(super) fn check_cpu(cpu: &Cpu, host: &HostInfo, findings: &mut Vec<Finding>) {
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
      findings.push(Finding::error(format!("{source} needs {feature}, but the host CPU ({}) lacks it", host.model_name)));
    }
  }
  if let Some(model) = model {
    for feature in model.newer {
      if host.flags.contains(*feature) && !cpu.features.hide.iter().any(|h| h == feature) {
        findings
          .push(Finding::warning(format!("the host exposes {feature}, which {} does not have; add it to cpu.features.hide", model.name)));
      }
    }
  }
}
