use needy_config::Source;

use super::*;

#[test]
fn firecracker_env_carries_the_recipe_identity() {
  let recipe = needy_config::recipe::load(&Source::text("extends: builtin:gce-ubuntu-22.04\ncpu: { hypervisor: hidden }", ".")).unwrap();
  let compiled = needy_core::recipe::compile(&recipe, &HostInfo::default(), 7);

  let env = firecracker_env(&compiled.identity);

  assert_eq!(env["NEEDY_HIDE_HYPERVISOR"], "1");
  assert_eq!(env["NEEDY_PCI_HOST_BRIDGE"], "8086:1237:02");
  assert_eq!(env["NEEDY_PCI_ISA_BRIDGE"], "8086:7110:03");
  let smbios = &env["NEEDY_SMBIOS"];
  assert!(smbios.contains("sys_vendor=Google;"), "{smbios}");
  assert!(smbios.contains("product_name=Google Compute Engine"), "{smbios}");
  assert!(smbios.contains("chassis_type=1"), "{smbios}");
  assert!(smbios.contains("bios_date=01/01/2011"), "{smbios}");
  assert!(smbios.contains(&format!("uuid={}", compiled.identity.smbios.as_ref().unwrap().uuid)), "{smbios}");
}

#[test]
fn smbios_values_cannot_end_a_field_early() {
  let identity = VmIdentity {
    smbios: Some(needy_core::recipe::Smbios {
      product_name: Some("a;b".into()),
      uuid: "u".into(),
      ..Default::default()
    }),
    ..Default::default()
  };
  assert_eq!(firecracker_env(&identity)["NEEDY_SMBIOS"], "product_name=a,b;uuid=u");
}

#[test]
fn a_profile_is_named_after_its_directory() {
  assert_eq!(profile_name(Path::new("/home/user/needy/examples/smoke")), "smoke");
}
