use needy_config::Source;
use needy_core::recipe::HostInfo;

use super::*;

const HOST_CPUINFO: &str = "processor\t: 0\nmodel name\t: Host CPU\nflags\t\t: fpu rdrand avx2\n\n";

/// A host root in `dir` with `/proc/cpuinfo` and, as asked, DMI, hwmon and
/// lxcfs's views.
fn host(dir: &Path, dmi: bool, hwmon: bool, lxcfs: bool) -> Host {
  std::fs::create_dir_all(dir.join("proc")).unwrap();
  std::fs::write(dir.join("proc/cpuinfo"), HOST_CPUINFO).unwrap();
  if dmi {
    std::fs::create_dir_all(dir.join("sys/class/dmi/id")).unwrap();
  }
  if hwmon {
    std::fs::create_dir_all(dir.join("sys/class/hwmon")).unwrap();
  }
  if lxcfs {
    for view in LXCFS_VIEWS {
      let path = dir.join(format!("var/lib/lxcfs{view}").trim_start_matches('/'));
      std::fs::create_dir_all(path.parent().unwrap()).unwrap();
      std::fs::write(path, "").unwrap();
    }
  }
  Host::read(dir)
}

fn recipe(extra: &str) -> CompiledRecipe {
  let text = format!("extends: builtin:gce-ubuntu-22.04\n{extra}");
  let recipe = needy_config::recipe::load(&Source::text(&text, ".")).unwrap();
  needy_core::recipe::compile(&recipe, &HostInfo::default(), 7)
}

const SENSORS: &str = "devices: { sensors: [{ chip: coretemp, idle_c: 40 }] }\n";

#[test]
fn the_recipes_cpuinfo_is_bound_over_the_hosts() {
  let root = tempfile::tempdir().unwrap();
  let run = tempfile::tempdir().unwrap();
  let host = host(root.path(), false, false, false);

  let binds = prepare(run.path(), Some(&recipe("")), 2, &host, OciRuntime::Runc).unwrap();

  let cpuinfo = run.path().join("identity/cpuinfo");
  assert_eq!(binds, [format!("{}:/proc/cpuinfo:ro", cpuinfo.display())]);
  let text = std::fs::read_to_string(cpuinfo).unwrap();
  assert_eq!(text.matches("model name\t: Intel(R) Xeon(R) CPU @ 2.80GHz").count(), 2, "{text}");
}

#[test]
fn dmi_and_hwmon_are_bound_where_the_host_has_them_to_cover() {
  let root = tempfile::tempdir().unwrap();
  let run = tempfile::tempdir().unwrap();
  let host = host(root.path(), true, true, false);

  let binds = prepare(run.path(), Some(&recipe(SENSORS)), 1, &host, OciRuntime::Runc).unwrap();

  let identity = run.path().join("identity");
  assert!(binds.contains(&format!("{}:/sys/class/dmi/id:ro", identity.join("dmi").display())), "{binds:?}");
  assert!(binds.contains(&format!("{}:/sys/class/hwmon:ro", identity.join("hwmon").display())), "{binds:?}");
  assert_eq!(std::fs::read_to_string(identity.join("dmi/sys_vendor")).unwrap(), "Google\n");
  assert_eq!(std::fs::read_to_string(identity.join("hwmon/hwmon0/temp1_input")).unwrap(), "40000\n");
}

#[test]
fn a_host_without_dmi_or_hwmon_gets_neither_bound_and_says_so() {
  let root = tempfile::tempdir().unwrap();
  let run = tempfile::tempdir().unwrap();
  let host = host(root.path(), false, false, true);
  let recipe = recipe(SENSORS);

  let binds = prepare(run.path(), Some(&recipe), 1, &host, OciRuntime::Runc).unwrap();
  let gaps = gaps(Some(&recipe), OciRuntime::Runc, &host);

  assert!(!binds.iter().any(|b| b.contains("/sys/class")), "{binds:?}");
  let fields: Vec<_> = gaps.iter().filter_map(|g| g.field).collect();
  assert!(fields.contains(&IdentityField::Dmi) && fields.contains(&IdentityField::Hwmon), "{gaps:?}");
}

#[test]
fn lxcfs_views_are_bound_and_never_over_cpuinfo() {
  let root = tempfile::tempdir().unwrap();
  let run = tempfile::tempdir().unwrap();
  let host = host(root.path(), false, false, true);

  let binds = prepare(run.path(), None, 2, &host, OciRuntime::Runc).unwrap();

  assert_eq!(binds.len(), LXCFS_VIEWS.len());
  let meminfo = root.path().join("var/lib/lxcfs/proc/meminfo");
  assert!(binds.contains(&format!("{}:/proc/meminfo:ro", meminfo.display())), "{binds:?}");
  assert!(!binds.iter().any(|b| b.ends_with(":/proc/cpuinfo:ro")), "{binds:?}");
}

#[test]
fn without_lxcfs_the_resource_views_are_a_gap() {
  let root = tempfile::tempdir().unwrap();
  let host = host(root.path(), true, true, false);

  let gaps = gaps(None, OciRuntime::Runc, &host);

  let lxcfs: Vec<&Gap> = gaps.iter().filter(|g| g.message.contains("lxcfs is not running")).collect();
  assert_eq!(lxcfs.len(), 1, "{gaps:?}");
  assert_eq!(lxcfs[0].field, None);
}

#[test]
fn the_cpuid_instruction_pci_and_kernel_are_never_the_recipes() {
  let root = tempfile::tempdir().unwrap();
  let host = host(root.path(), true, true, true);
  let recipe = recipe("devices: { pci: { host_bridge: \"8086:1237\" } }\n");

  let fields: Vec<_> = gaps(Some(&recipe), OciRuntime::Runc, &host).into_iter().filter_map(|g| g.field).collect();

  assert_eq!(
    fields,
    [
      IdentityField::Cpuid,
      IdentityField::Pci,
      IdentityField::Kernel
    ]
  );
}

#[test]
fn an_overheat_rewrites_every_reading_and_raises_the_alarm() {
  let root = tempfile::tempdir().unwrap();
  let run = tempfile::tempdir().unwrap();
  let host = host(root.path(), false, true, false);
  prepare(run.path(), Some(&recipe(SENSORS)), 1, &host, OciRuntime::Runc).unwrap();

  overheat(run.path(), 101_000).unwrap();

  let device = run.path().join("identity/hwmon/hwmon0");
  assert_eq!(std::fs::read_to_string(device.join("temp1_input")).unwrap(), "101000\n");
  assert_eq!(std::fs::read_to_string(device.join("temp2_input")).unwrap(), "101000\n");
  assert_eq!(std::fs::read_to_string(device.join("temp1_crit_alarm")).unwrap(), "1\n");
  assert_eq!(std::fs::read_to_string(device.join("temp1_label")).unwrap(), "Package id 0\n");
}

#[test]
fn under_gvisor_dmi_and_hwmon_are_bound_whatever_the_host_has_and_no_lxcfs_view_is() {
  let root = tempfile::tempdir().unwrap();
  let run = tempfile::tempdir().unwrap();
  let host = host(root.path(), false, false, true);

  let binds = prepare(run.path(), Some(&recipe(SENSORS)), 1, &host, OciRuntime::Runsc).unwrap();

  let targets: Vec<&str> = binds.iter().map(|b| b.split(':').nth(1).unwrap()).collect();
  assert_eq!(targets, ["/proc/cpuinfo", "/sys/class/dmi/id", "/sys/class/hwmon"]);
}

#[test]
fn gvisor_has_its_own_resource_views_and_kernel() {
  let root = tempfile::tempdir().unwrap();
  let host = host(root.path(), false, false, false);
  let recipe = recipe("");

  let gaps = gaps(Some(&recipe), OciRuntime::Runsc, &host);

  let messages: Vec<&str> = gaps.iter().map(|g| g.message.as_str()).collect();
  assert!(!messages.iter().any(|m| m.contains("lxcfs")), "{messages:?}");
  assert!(messages.iter().any(|m| m.contains("gVisor has no /proc/diskstats")), "{messages:?}");
  assert!(messages.contains(&"uname and /proc/version show gVisor's (4.19.0-gvisor) kernel"), "{messages:?}");
  assert!(!gaps.iter().any(|g| g.field == Some(IdentityField::Dmi)), "{gaps:?}");
}
