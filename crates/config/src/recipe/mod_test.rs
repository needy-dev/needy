use std::path::Path;

use super::*;

fn load_text(yaml: &str) -> Result<Recipe, crate::Error> {
  load(&Source::text(yaml, "/base"))
}

#[test]
fn every_built_in_loads() {
  let names: Vec<&str> = crate::builtin::recipes().collect();
  assert_eq!(names, ["gce-ubuntu-22.04", "dell-optiplex-ubuntu-24.04"]);

  for name in names {
    load(&Source::builtin(name)).unwrap_or_else(|err| panic!("{name}: {err}"));
  }
}

#[test]
fn a_built_in_recipe_has_its_identity() {
  let r = load(&Source::reference("builtin:dell-optiplex-ubuntu-24.04", Path::new("/ignored"))).unwrap();

  assert_eq!(
    r.cpu.signature,
    Some(Signature {
      family: 6,
      model: 151,
      stepping: 2
    })
  );
  assert_eq!(r.cpu.hypervisor, Some(Hypervisor::Hidden));
  assert_eq!(r.memory.map(|m| m.mib()), Some(16384));
  assert_eq!(r.os.uptime.map(|d| d.as_secs()), Some(2 * 86400 + 7 * 3600));
  assert_eq!(r.os.hostname.as_ref().map(|t| t.as_str()), Some("optiplex-{hash}"));
  let dmi = r.firmware.dmi.as_ref().unwrap();
  assert_eq!(dmi.chassis_type, Some(ChassisType::Name("tower".into())));
  assert_eq!(dmi.bios.version.as_deref(), Some("1.21.0"));
  assert_eq!(r.devices.sensors.len(), 3);
  assert_eq!(r.devices.sensors[2].idle_c, 27.8);
  assert_eq!(r.network.mac.as_ref().map(|t| t.as_str()), Some("b0:7b:25:{seq}:12:34"));
}

// One merge model: the child's maps merge into the built-in's, and its
// scalars and lists replace the built-in's.
#[test]
fn extends_a_built_in_and_merges() {
  let r = load_text(
    r#"
extends: builtin:gce-ubuntu-22.04
cpu:
  signature: { family: 6, model: 79, stepping: 0 }
  features: { hide: [avx_vnni] }
memory: 16GiB
os:
  kernel: { build: { host: builder } }
devices:
  pci:
    extra: []
"#,
  )
  .unwrap();

  assert_eq!(r.cpu.brand.as_deref(), Some("Intel(R) Xeon(R) CPU @ 2.20GHz"));
  assert_eq!(
    r.cpu.signature,
    Some(Signature {
      family: 6,
      model: 79,
      stepping: 0
    })
  );
  assert_eq!(r.cpu.hypervisor, Some(Hypervisor::Visible));
  assert_eq!(r.cpu.features.hide, ["avx_vnni"]);
  assert_eq!(r.memory.map(|m| m.mib()), Some(16384));
  assert_eq!(r.os.image.as_deref(), Some("ubuntu:22.04"));
  assert_eq!(r.os.kernel.release.as_deref(), Some("6.5.0-1020-gcp"));
  assert_eq!(r.os.kernel.build.user.as_deref(), Some("buildd"));
  assert_eq!(r.os.kernel.build.host.as_deref(), Some("builder"));
  assert_eq!(r.devices.pci.host_bridge.as_deref(), Some("8086:1237:02"));
  assert!(r.devices.pci.extra.is_empty());
  assert_eq!(r.network.metadata.as_ref().and_then(|m| m.provider.as_deref()), Some("gce"));
  assert_eq!(r.base_dir, Path::new("/base"));
}

#[test]
fn a_file_recipe_extends_a_file() {
  let dir = tempfile::tempdir().unwrap();
  std::fs::write(dir.path().join("base.recipe.yaml"), "cpu: { features: { hide: [serialize, gfni] } }\nmemory: 2048\n").unwrap();
  std::fs::write(dir.path().join("child.recipe.yaml"), "extends: base.recipe.yaml\ncpu: { features: { require: [avx2] } }\n").unwrap();

  let r = load(&Source::file(dir.path().join("child.recipe.yaml"))).unwrap();

  assert_eq!(r.cpu.features.hide, ["serialize", "gfni"]);
  assert_eq!(r.cpu.features.require, ["avx2"]);
  assert_eq!(r.memory.map(|m| m.mib()), Some(2048));
  assert_eq!(r.base_dir, dir.path());
}

#[test]
fn an_unknown_built_in_is_named() {
  let err = load_text("extends: builtin:nope").unwrap_err().to_string();

  assert_eq!(err, "recipe (text in /base): no built-in recipe named \"nope\"");
}

#[test]
fn unknown_keys_are_rejected() {
  let err = load_text("cpu: { brnd: Xeon }").unwrap_err().to_string();
  assert!(err.contains("cpu.brnd: unknown field `brnd`"), "{err}");

  // The recipe note's raw `firecracker:` overrides are not implemented.
  let err = load_text("firecracker: { cpu_template: ./x.json }").unwrap_err().to_string();
  assert!(err.contains("unknown field `firecracker`"), "{err}");
}

#[test]
fn bad_values_are_rejected() {
  let err = load_text("memory: 2Q").unwrap_err().to_string();
  assert!(err.contains("memory: invalid size unit \"Q\""), "{err}");

  let err = load_text("os: { uptime: a while }").unwrap_err().to_string();
  assert!(err.contains("os.uptime: invalid duration"), "{err}");

  let err = load_text("cpu: { hypervisor: shown }").unwrap_err().to_string();
  assert!(err.contains("unknown variant `shown`"), "{err}");
}
