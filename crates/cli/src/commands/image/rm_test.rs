use super::*;

#[test]
fn an_image_is_found_by_base_by_its_reference_or_by_its_directory_name() {
  let images = tempfile::tempdir().unwrap();
  let base = vm::base_dir(images.path(), ActorDriver::LinuxFirecracker);
  let custom = vm::dir_of(images.path(), ActorDriver::LinuxFirecracker, "needy-custom-jq");
  std::fs::create_dir_all(&base).unwrap();
  std::fs::create_dir_all(&custom).unwrap();
  let name = custom.file_name().unwrap().to_str().unwrap();

  assert_eq!(find(images.path(), ActorDriver::LinuxFirecracker, "base"), Some(base));
  assert_eq!(find(images.path(), ActorDriver::LinuxFirecracker, "needy-custom-jq"), Some(custom.clone()));
  assert_eq!(find(images.path(), ActorDriver::LinuxFirecracker, "needy-custom-jq:latest"), Some(custom.clone()));
  assert_eq!(find(images.path(), ActorDriver::LinuxFirecracker, name), Some(custom));
  assert_eq!(find(images.path(), ActorDriver::MacosVz, "base"), None, "another driver's");
  assert_eq!(find(images.path(), ActorDriver::LinuxFirecracker, "needy-never-built"), None);
}

#[test]
fn a_name_that_leaves_the_drivers_directory_is_no_image() {
  let images = tempfile::tempdir().unwrap();
  std::fs::create_dir_all(vm::base_dir(images.path(), ActorDriver::MacosVz)).unwrap();
  std::fs::create_dir_all(images.path().join("linux-firecracker")).unwrap();

  assert_eq!(find(images.path(), ActorDriver::LinuxFirecracker, "../macos-vz/base"), None);
  assert_eq!(find(images.path(), ActorDriver::LinuxFirecracker, ".."), None);
}

#[test]
fn every_guest_disk_but_the_current_ones_is_old() {
  let disks = tempfile::tempdir().unwrap();
  for name in [
    "0123456789abcdef.squashfs",
    "fedcba9876543210.squashfs",
    ".tmpAbc",
  ] {
    std::fs::write(disks.path().join(name), "").unwrap();
  }

  let old = old_guest_disks(disks.path(), Some("0123456789abcdef.squashfs")).unwrap();

  assert_eq!(old, [disks.path().join("fedcba9876543210.squashfs")]);
  assert_eq!(old_guest_disks(&disks.path().join("none"), None).unwrap(), Vec::<PathBuf>::new());
}
