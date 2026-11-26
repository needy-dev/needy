use super::*;

// ROOT CAUSE:
//
// If two checkouts on one host ran the harness (a worktree beside the main
// checkout), each one's cells booted the images the other had built last,
// because every checkout tagged its images `latest` while each kept its
// freshness stamps in its own work directory, so a stamp said fresh for an
// image the other checkout had replaced.
//
// Before the fix, every checkout used `needy-e2e-actor:latest`.
// The fix tags the images with the checkout's directory name.
#[test]
fn two_checkouts_build_images_of_their_own() {
  let main = Images::of_checkout(Path::new("/Users/user/src/needy"));
  let worktree = Images::of_checkout(Path::new("/tmp/needy-main-wt"));

  assert_eq!(main.actor, "needy-e2e-actor:needy");
  assert_eq!(main.executor, "needy-e2e-executor:needy");
  assert_eq!(main.agents, "needy-e2e-agents:needy");
  assert_eq!(worktree.actor, "needy-e2e-actor:needy-main-wt");
}

// A run host runs the harness its build host compiled in `DIR/src`, so both
// name the images the run host loads alike.
#[test]
fn a_shipped_tree_names_its_images_after_src() {
  assert_eq!(Images::shipped(), Images::of_checkout(Path::new("/home/user/needy-e2e/src")));
}

#[test]
fn a_checkout_name_becomes_a_valid_tag() {
  assert_eq!(tag("needy"), "needy");
  assert_eq!(tag("my repo@2"), "my-repo-2");
  assert_eq!(tag(".hidden"), "_.hidden");
  assert_eq!(tag(""), "_");
  assert_eq!(tag(&"x".repeat(200)).len(), 128);
}

// ROOT CAUSE:
//
// If a run host had no /sys/class/hwmon, `linux-container.runc/chaos/fault=sensor`
// failed, because the harness judged the cell only by its driver while the
// actor deliberately presents no sensors there and records a hwmon gap
// (`needy_core_actor::identity::gaps`).
//
// Before the fix, the cell ran and failed on such hosts.
// The fix reads the host as the actor does and reports the cell not
// covered with the recorded gap; runsc synthesizes its own hwmon and stays
// covered.
#[test]
fn a_host_without_hwmon_has_no_runc_sensors() {
  let root = std::env::temp_dir().join(format!("xtask-sensors-{}", std::process::id()));
  let _ = std::fs::remove_dir_all(&root);
  std::fs::create_dir_all(&root).unwrap();

  let without = container_sensors(&root);
  std::fs::create_dir_all(root.join("sys/class/hwmon")).unwrap();
  let with = container_sensors(&root);
  // Best effort: a leftover directory under the temp dir is harmless.
  let _ = std::fs::remove_dir_all(&root);

  let runc = without[&Driver::Container(Oci::Runc)].as_ref().unwrap_err();
  assert!(runc.contains("no /sys/class/hwmon"), "{runc}");
  assert_eq!(without[&Driver::Container(Oci::Runsc)], Ok(()));
  assert_eq!(with[&Driver::Container(Oci::Runc)], Ok(()));
  assert_eq!(with[&Driver::Container(Oci::Runsc)], Ok(()));
}
