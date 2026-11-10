use needy_config::Source;
use needy_config::recipe::Signature;
use needy_config::validate::Severity;

use super::cpu::{encode_signature, feature_bit};
use super::*;

fn load(yaml: &str) -> Recipe {
  needy_config::recipe::load(&Source::text(yaml, ".")).unwrap()
}

/// The captured `/proc/cpuinfo` of a Raptor Lake host (see `testdata/SOURCES.md`).
fn raptor_lake() -> HostInfo {
  cpuinfo("cpuinfo-raptor-lake.txt")
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
  let recipe = load(GCE);
  let c = compile(&recipe, &HostInfo::default(), 7);
  assert_eq!(c.vcpus, 4);
  assert_eq!(c.mem_mib, 16384);
  let identity = &c.identity;
  assert_eq!(identity.cpu_brand.as_deref(), Some("Intel(R) Xeon(R) CPU @ 2.20GHz"));
  assert!(identity.hide_hypervisor);
  assert_eq!(identity.pci_host_bridge.map(|id| id.to_string()).as_deref(), Some("8086:1237:02"));
  assert_eq!(identity.pci_isa_bridge.map(|id| id.to_string()).as_deref(), Some("8086:7110:03"));
  let smbios = identity.smbios.as_ref().unwrap();
  assert_eq!(smbios.sys_vendor.as_deref(), Some("Google"));
  assert_eq!(smbios.product_name.as_deref(), Some("Google Compute Engine"));
  assert_eq!(smbios.chassis_type, Some(1));
  assert_eq!(smbios.bios_date.as_deref(), Some("01/01/2011"));
  assert_eq!(smbios.uuid.len(), 36, "{smbios:?}");
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
  let again = compile(&recipe, &HostInfo::default(), 7);
  assert_eq!(again.hostname, c.hostname);
  assert_eq!(again.identity, c.identity);
  let other = compile(&recipe, &HostInfo::default(), 8);
  assert_ne!(other.hostname, c.hostname);
  assert!(c.findings.is_empty(), "{:?}", c.findings);
}

#[test]
fn cascade_lake_on_raptor_lake() {
  let recipe = load("cpu: { signature: { family: 6, model: 85, stepping: 7 }, features: { hide: [serialize] } }");
  let c = compile(&recipe, &raptor_lake(), 1);
  let errors: Vec<_> = c.findings.iter().filter(|f| f.severity == Severity::Error).collect();
  assert!(errors.iter().any(|f| f.message.contains("avx512f")), "{errors:?}");
  let warnings: Vec<_> = c.findings.iter().filter(|f| f.severity == Severity::Warning).map(|f| f.message.clone()).collect();
  assert!(warnings.iter().any(|w| w.contains("avx_vnni")), "{warnings:?}");
  assert!(!warnings.iter().any(|w| w.contains("serialize")), "hidden features do not warn");
}

#[test]
fn raptor_lake_identity_on_raptor_lake_is_clean() {
  let recipe = load("cpu: { signature: { family: 6, model: 183, stepping: 1 }, features: { require: [avx2] } }");
  assert!(compile(&recipe, &raptor_lake(), 1).findings.is_empty());
}

/// A real host's `/proc/cpuinfo` from `testdata` (see `testdata/SOURCES.md`).
fn cpuinfo(fixture: &str) -> HostInfo {
  let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/recipe/testdata").join(fixture);
  HostInfo::from_cpuinfo(&std::fs::read_to_string(path).unwrap())
}

// ROOT CAUSE:
//
// If a recipe named SSE3 as Linux's `/proc/cpuinfo` does (`pni`), it got
// "unknown feature", and `sse3` never matched a host's flags, because the
// feature table used the name `sse3`, which Linux does not show.
//
// Before the fix, `hide: [pni]` was reported as unknown and left CPUID
// alone, and `require: [sse3]` failed on every host.
// The fix keeps every feature name the one `/proc/cpuinfo` shows.
#[test]
fn sse3_is_named_pni_as_in_proc_cpuinfo() {
  let host = cpuinfo("cpuinfo-raptor-lake.txt");
  let recipe = load("cpu: { signature: { family: 6, model: 183, stepping: 1 }, features: { require: [pni], hide: [pni] } }");

  let c = compile(&recipe, &host, 1);

  assert!(c.findings.is_empty(), "{:?}", c.findings);
  let modifiers = c.cpu_template["cpuid_modifiers"].as_array().unwrap();
  let leaf1 = modifiers.iter().find(|m| m["leaf"] == "0x1").unwrap()["modifiers"].as_array().unwrap();
  let ecx = leaf1.iter().find(|m| m["register"] == "ecx").unwrap()["bitmap"].as_str().unwrap();
  assert_eq!(ecx, format!("0b{}0", "x".repeat(31)), "pni is CPUID.01H:ECX[0]");
}

// ROOT CAUSE:
//
// If the host CPU was hybrid, as the Raptor Lake host in `testdata` is,
// `check_cpu` never warned about `hybrid_cpu`, because Linux has no
// `/proc/cpuinfo` name for CPUID.07H:EDX[15]. The guest cannot see that bit
// either: KVM leaves it out of the CPUID it supports, so hiding it changed
// nothing.
//
// Before the fix, `hide: [hybrid_cpu]` was accepted silently and the warning
// was dead code.
// The fix drops `hybrid_cpu` from the feature table and the model checks,
// so a recipe that names it is told the feature is unknown.
#[test]
fn hybrid_cpu_is_not_a_recipe_feature() {
  let host = cpuinfo("cpuinfo-raptor-lake.txt");
  assert!(!host.flags.contains("hybrid_cpu"), "a hybrid host shows no such flag");
  let recipe = load("cpu: { signature: { family: 6, model: 79, stepping: 0 }, features: { hide: [hybrid_cpu] } }");

  let c = compile(&recipe, &host, 1);

  let messages: Vec<_> = c.findings.iter().map(|f| f.message.clone()).collect();
  assert!(messages.contains(&"cpu.features.hide: unknown feature \"hybrid_cpu\"".to_string()), "{messages:?}");
}

#[test]
fn vendor_disagreement_warns() {
  let recipe = load("extends: builtin:gce-ubuntu-22.04\nnetwork: { mac: \"0e:11:22:33:44:55\" }\nos: { kernel: { release: 6.8.0-1-aws } }");
  let c = compile(&recipe, &HostInfo::default(), 1);
  assert!(c.findings.iter().any(|f| f.message.contains("machine identity disagrees")), "{:?}", c.findings);
}

fn builtin(name: &str, seed: u64) -> CompiledRecipe {
  compile(&needy_config::recipe::load(&Source::builtin(name)).unwrap(), &HostInfo::default(), seed)
}

// ROOT CAUSE:
//
// If the GCE built-in was compiled, its kernel version string and build host
// were not the ones the cited Ubuntu build carries, because they were typed
// in rather than read from that build's `vmlinuz`.
//
// Before the fix, the version said `Fri Mar  1 10:04:32 UTC 2024` and the
// host `lcy02-amd64-033`.
// The fix keeps both as `linux-image-unsigned-6.5.0-1020-gcp` reports them.
#[test]
fn the_gce_builtin_kernel_is_the_cited_ubuntu_build() {
  let c = builtin("gce-ubuntu-22.04", 1);

  assert_eq!(c.kernel_build_env["NEEDY_KERNEL_VERSION"], "#20~22.04.1-Ubuntu SMP Wed May  1 02:03:24 UTC 2024");
  assert_eq!(c.kernel_build_env["KBUILD_BUILD_HOST"], "lcy02-amd64-115");
}

// ROOT CAUSE:
//
// If the GCE built-in was compiled, its system serial was `GoogleCloud-`
// and 8 hex digits unrelated to the system UUID, and it had no board
// serial, because `{hash}` was the only placeholder; real GCE serials are
// `GoogleCloud-` and `Board-GoogleCloud-` followed by the system UUID's 32
// upper-case hex digits.
//
// Before the fix, the serial was `GoogleCloud-93f88501`.
// The fix derives both serials from the UUID through `{uuid_hex}`.
#[test]
fn the_gce_builtin_serials_carry_the_system_uuid() {
  let c = builtin("gce-ubuntu-22.04", 7);
  let smbios = c.identity.smbios.unwrap();

  let hex = smbios.uuid.replace('-', "").to_ascii_uppercase();
  assert_eq!(hex.len(), 32);
  assert_eq!(smbios.serial.unwrap(), format!("GoogleCloud-{hex}"));
  assert_eq!(smbios.board_serial.unwrap(), format!("Board-GoogleCloud-{hex}"));
}

// ROOT CAUSE:
//
// If a session's seed gave `{seq}` the value 00 or 01, the GCE built-in's
// MAC encoded 10.128.0.0 or 10.128.0.1, the subnet's network and gateway
// addresses, which no VM gets, because `{seq}` covered all 256 values.
//
// Before the fix, seed 7 gave `42:01:0a:80:00:01`.
// The fix keeps `{seq}` from 02 to ff.
#[test]
fn the_gce_builtin_mac_is_never_the_subnets_network_or_gateway() {
  for seed in 0..4096 {
    let mac = builtin("gce-ubuntu-22.04", seed).network_mac.unwrap();
    assert!(mac.starts_with("42:01:0a:80:00:"), "{mac}");
    assert!(!mac.ends_with(":00") && !mac.ends_with(":01"), "seed {seed}: {mac}");
  }
}

#[test]
fn builtins_compile() {
  for name in needy_config::builtin::recipes() {
    let r = needy_config::recipe::load(&Source::builtin(name)).unwrap();
    let c = compile(&r, &HostInfo::default(), 3);
    assert!(c.findings.is_empty(), "{name}: {:?}", c.findings);
  }
  let dell = compile(&load("extends: builtin:dell-optiplex-ubuntu-24.04"), &HostInfo::default(), 3);
  assert_eq!(dell.sensors.len(), 3);
  assert_eq!(dell.sensors[0].idle_mc, 34000);
  assert_eq!(dell.identity.smbios.as_ref().unwrap().chassis_type, Some(7));
}

#[test]
fn unknown_features_and_bad_pci_ids_are_findings() {
  let r = load("cpu: { features: { hide: [not_a_flag] } }\ndevices: { pci: { host_bridge: zz } }");
  let c = compile(&r, &HostInfo::default(), 0);
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

/// The CPUID entries a host's KVM supports, from a fixture in `testdata`
/// (see `testdata/SOURCES.md`).
fn kvm_cpuid(fixture: &str) -> std::collections::BTreeSet<(u32, u32)> {
  let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/recipe/testdata").join(fixture);
  let hex = |s: &str| u32::from_str_radix(s.trim_start_matches("0x"), 16).unwrap();
  std::fs::read_to_string(&path)
    .unwrap()
    .lines()
    .map(|line| {
      let (leaf, subleaf) = line.split_once(' ').unwrap();
      (hex(leaf), hex(subleaf))
    })
    .collect()
}

/// The modifiers of `template` as `(leaf, subleaf)` strings.
fn modified_entries(template: &serde_json::Value) -> Vec<(String, String)> {
  template["cpuid_modifiers"]
    .as_array()
    .unwrap()
    .iter()
    .map(|m| (m["leaf"].as_str().unwrap().to_string(), m["subleaf"].as_str().unwrap().to_string()))
    .collect()
}

// ROOT CAUSE:
//
// If a recipe hid a feature in a CPUID entry the host's KVM does not
// support, such as `avx_vnni` (leaf 7, subleaf 1) on Cascade Lake, the actor
// never booted, because the template still modified that entry and
// Firecracker refuses a template that changes an entry KVM did not report.
//
// Before the fix, `examples/opencode/gce.recipe.yaml` failed on a Cascade
// Lake host with "Template changes a CPUID entry not supported by KVM: Leaf: 7,
// Subleaf: 1".
// The fix keeps hiding a feature the host does not provide a no-op: the
// template modifies only entries the host's KVM supports, and the recipe
// says which hides it left out.
#[test]
fn hiding_a_feature_in_a_cpuid_entry_the_hosts_kvm_lacks_emits_no_modifier() {
  let recipe = load(GCE);
  let host = HostInfo {
    kvm_cpuid: Some(kvm_cpuid("kvm-cpuid-cascade-lake.txt")),
    ..HostInfo::default()
  };

  let c = compile(&recipe, &host, 7);

  let entries = modified_entries(&c.cpu_template);
  assert!(!entries.contains(&("0x7".into(), "0x1".into())), "{entries:?}");
  assert!(entries.contains(&("0x7".into(), "0x0".into())), "serialize is still hidden: {entries:?}");
  assert!(entries.contains(&("0x40000000".into(), "0x0".into())), "{entries:?}");
  let messages: Vec<_> = c.findings.iter().map(|f| f.to_string()).collect();
  assert_eq!(
    messages,
    ["warning: cpu.features.hide: avx_vnni has no effect: this host's KVM has no CPUID leaf 0x7, subleaf 0x1, so the guest cannot see it"]
  );
}

#[test]
fn hiding_a_feature_in_a_cpuid_entry_the_hosts_kvm_supports_modifies_it() {
  let recipe = load(GCE);
  let host = HostInfo {
    kvm_cpuid: Some(kvm_cpuid("kvm-cpuid-raptor-lake.txt")),
    ..HostInfo::default()
  };

  let c = compile(&recipe, &host, 7);

  assert!(modified_entries(&c.cpu_template).contains(&("0x7".into(), "0x1".into())));
  assert!(c.findings.is_empty(), "{:?}", c.findings);
}
