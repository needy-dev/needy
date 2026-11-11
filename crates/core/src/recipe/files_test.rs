use super::*;

/// The first processor's block of a Raptor Lake host; see
/// `testdata/SOURCES.md`.
const HOST: &str = include_str!("testdata/cpuinfo-raptor-lake.txt");

fn view() -> CpuView {
  CpuView {
    brand: Some("Intel(R) Xeon(R) CPU @ 2.80GHz".into()),
    signature: Some((6, 85, 7)),
    hide: vec!["avx_vnni".into(), "sha_ni".into()],
    hypervisor: Some(true),
    threads_per_core: 2,
  }
}

/// The value of `key` in each processor block of `cpuinfo`.
fn values<'a>(cpuinfo: &'a str, key: &str) -> Vec<&'a str> {
  cpuinfo.lines().filter_map(|line| line.split_once(':')).filter(|(k, _)| k.trim_end() == key).map(|(_, v)| v.trim()).collect()
}

#[test]
fn cpuinfo_has_one_block_per_recipe_vcpu() {
  let cpuinfo = cpuinfo(HOST, &view(), 2);

  assert_eq!(values(&cpuinfo, "processor"), ["0", "1"]);
  assert_eq!(values(&cpuinfo, "siblings"), ["2", "2"]);
}

#[test]
fn cpuinfo_shows_the_recipes_brand_and_signature() {
  let cpuinfo = cpuinfo(HOST, &view(), 2);

  assert_eq!(values(&cpuinfo, "model name"), ["Intel(R) Xeon(R) CPU @ 2.80GHz"; 2]);
  assert_eq!(values(&cpuinfo, "cpu family"), ["6", "6"]);
  assert_eq!(values(&cpuinfo, "model"), ["85", "85"]);
  assert_eq!(values(&cpuinfo, "stepping"), ["7", "7"]);
}

#[test]
fn cpuinfo_puts_two_threads_on_one_core_as_the_topology_says() {
  let cpuinfo = cpuinfo(HOST, &view(), 2);

  assert_eq!(values(&cpuinfo, "core id"), ["0", "0"]);
  assert_eq!(values(&cpuinfo, "cpu cores"), ["1", "1"]);
  assert_eq!(values(&cpuinfo, "apicid"), ["0", "1"]);
}

#[test]
fn cpuinfo_leaves_out_the_hidden_flags_and_shows_a_visible_hypervisor_after_rdrand() {
  let cpuinfo = cpuinfo(HOST, &view(), 1);

  let flags: Vec<&str> = values(&cpuinfo, "flags")[0].split(' ').collect();
  assert!(!flags.contains(&"avx_vnni") && !flags.contains(&"sha_ni"), "{flags:?}");
  assert!(flags.contains(&"avx2"), "{flags:?}");
  let rdrand = flags.iter().position(|f| *f == "rdrand").unwrap();
  assert_eq!(flags[rdrand + 1], "hypervisor");
}

#[test]
fn a_hidden_hypervisor_is_not_a_flag() {
  let host = HOST.replace(" rdrand ", " rdrand hypervisor ");
  let cpu = CpuView {
    hypervisor: Some(false),
    ..view()
  };

  let cpuinfo = cpuinfo(&host, &cpu, 1);

  assert!(!values(&cpuinfo, "flags")[0].split(' ').any(|f| f == "hypervisor"));
}

#[test]
fn what_the_recipe_does_not_say_stays_the_hosts() {
  let cpuinfo = cpuinfo(HOST, &CpuView::default(), 1);

  assert_eq!(values(&cpuinfo, "model name"), ["13th Gen Intel(R) Core(TM) i5-13600KF"]);
  assert_eq!(values(&cpuinfo, "microcode"), ["0x132"]);
  assert_eq!(values(&cpuinfo, "flags"), values(HOST, "flags"));
}

#[test]
fn cpuinfo_keeps_the_kernels_key_padding() {
  let cpuinfo = cpuinfo(HOST, &view(), 1);

  assert!(cpuinfo.starts_with("processor\t: 0\nvendor_id\t: GenuineIntel\ncpu family\t: 6\nmodel\t\t: 85\n"), "{cpuinfo}");
  assert!(cpuinfo.contains("\npower management:\n"), "{cpuinfo}");
}

#[test]
fn the_serial_numbers_and_uuid_are_for_root_only_as_the_kernel_makes_them() {
  let modes: std::collections::BTreeMap<_, _> = dmi(&Smbios::default()).into_iter().map(|(name, _, mode)| (name, mode)).collect();

  assert_eq!((modes["product_uuid"], modes["product_serial"], modes["board_serial"]), (0o400, 0o400, 0o400));
  assert_eq!(modes["product_name"], 0o444);
}

#[test]
fn dmi_files_hold_the_recipes_values_and_an_empty_line_for_the_rest() {
  let smbios = Smbios {
    sys_vendor: Some("Google".into()),
    product_name: Some("Google Compute Engine".into()),
    uuid: "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0".into(),
    chassis_type: Some(1),
    ..Default::default()
  };

  let files: std::collections::BTreeMap<_, _> = dmi(&smbios).into_iter().map(|(name, content, _)| (name, content)).collect();

  assert_eq!(files["sys_vendor"], "Google\n");
  assert_eq!(files["product_name"], "Google Compute Engine\n");
  assert_eq!(files["product_uuid"], "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0\n");
  assert_eq!(files["chassis_type"], "1\n");
  assert_eq!(files["board_serial"], "\n");
  assert_eq!(files.len(), 13);
}

fn coretemp(idle_mc: i32) -> Vec<SensorSpec> {
  vec![SensorSpec {
    chip: "coretemp".into(),
    idle_mc,
    load_gain_mc: 0,
    noise_mc: 0,
    per_core: true,
  }]
}

#[test]
fn hwmon_has_the_package_and_one_core_per_vcpu_as_the_guest_kernel_does() {
  let files: std::collections::BTreeMap<_, _> = hwmon(&coretemp(38_000), 2, None).into_iter().collect();

  assert_eq!(files["hwmon0/name"], "coretemp\n");
  assert_eq!(files["hwmon0/temp1_label"], "Package id 0\n");
  assert_eq!(files["hwmon0/temp1_input"], "38000\n");
  assert_eq!(files["hwmon0/temp2_label"], "Core 0\n");
  assert_eq!(files["hwmon0/temp6_label"], "Core 4\n");
  assert_eq!(files["hwmon0/temp6_input"], "39000\n");
  assert_eq!(files["hwmon0/temp1_crit_alarm"], "0\n");
}

#[test]
fn an_hwmon_fault_sets_every_reading_and_raises_the_alarm() {
  let files: std::collections::BTreeMap<_, _> = hwmon(&coretemp(38_000), 1, Some(101_000)).into_iter().collect();

  assert_eq!(files["hwmon0/temp1_input"], "101000\n");
  assert_eq!(files["hwmon0/temp2_input"], "101000\n");
  assert_eq!(files["hwmon0/temp1_crit_alarm"], "1\n");
}

#[test]
fn without_a_coretemp_sensor_the_package_idles_at_the_guest_kernels_default() {
  let files: std::collections::BTreeMap<_, _> = hwmon(&[], 2, None).into_iter().collect();

  assert_eq!(files["hwmon0/temp1_input"], "38000\n");
}
