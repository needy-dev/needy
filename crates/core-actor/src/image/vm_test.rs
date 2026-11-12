use super::*;

#[test]
fn a_vm_image_made_from_an_oci_image_is_found_by_its_tagged_reference() {
  let images = Path::new("/home/user/.local/share/needy/images");

  let bare = dir_of(images, ActorDriver::MacosVz, "needy-custom-jq");
  let tagged = dir_of(images, ActorDriver::MacosVz, "needy-custom-jq:latest");

  assert_eq!(bare, tagged, "a bare name is its `latest` tag");
  assert_eq!(bare.parent(), Some(images.join("macos-vz").as_path()));
  let name = bare.file_name().unwrap().to_str().unwrap();
  assert!(name.starts_with("needy-custom-jq_latest-"), "{name}");
  assert_eq!(name.len(), "needy-custom-jq_latest-".len() + 12, "{name}");
}

#[test]
fn references_a_file_name_spells_alike_get_their_own_vm_images() {
  let images = Path::new("/images");

  let slash = dir_of(images, ActorDriver::LinuxFirecracker, "example.com/agent:1");
  let underscore = dir_of(images, ActorDriver::LinuxFirecracker, "example.com_agent:1");

  assert_ne!(slash, underscore);
  assert_ne!(dir_of(images, ActorDriver::LinuxFirecracker, "agent"), dir_of(images, ActorDriver::MacosVz, "agent"));
}

#[test]
fn a_missing_vm_image_made_from_an_oci_image_names_the_build_from_it() {
  let dir = tempfile::tempdir().unwrap();

  let error = MacosVz::of(dir.path(), Source::Reference("needy-custom-jq".into())).check().unwrap_err();

  assert_eq!(
    error.to_string(),
    format!("actor image is missing {}; run `needy image build --from needy-custom-jq`", dir.path().join("Image").display())
  );
}

#[test]
fn a_vm_image_of_another_base_abi_names_its_reference_and_the_base_to_rebuild_from() {
  let dir = tempfile::tempdir().unwrap();
  for file in ["Image", "initrd", "rootfs.ext4"] {
    std::fs::write(dir.path().join(file), "").unwrap();
  }
  write_manifest(dir.path(), ActorDriver::MacosVz, 2);

  let error = MacosVz::of(dir.path(), Source::Reference("needy-custom-jq".into())).check().unwrap_err().to_string();

  assert!(error.starts_with(&format!("the actor image needy-custom-jq (the VM image {}) has base ABI 2", dir.path().display())), "{error}");
  assert!(error.contains("rebuild it FROM a base image of base ABI 1"), "{error}");
  assert!(error.contains("then run `needy image build --from needy-custom-jq`"), "{error}");
}

/// Writes a manifest of `driver` and `base_abi` into `dir`.
fn write_manifest(dir: &Path, driver: ActorDriver, base_abi: u32) {
  let manifest = manifest::Manifest {
    version: "0.0.1".into(),
    driver,
    arch: arch(driver).into(),
    base_abi,
    source: None,
    sha256: None,
  };
  manifest::write(dir, &manifest).unwrap();
}

/// A complete `macos-vz` image in `dir`, but for its manifest.
fn macos_vz_files(dir: &Path) {
  for file in files(ActorDriver::MacosVz) {
    std::fs::write(dir.join(file), "").unwrap();
  }
}

#[test]
fn a_base_vm_image_with_its_manifest_is_accepted() {
  let dir = tempfile::tempdir().unwrap();
  macos_vz_files(dir.path());
  write_manifest(dir.path(), ActorDriver::MacosVz, base::ABI);

  MacosVz::new(dir.path()).check().unwrap();
}

#[test]
fn a_missing_base_vm_image_names_needy_image_pull_first_and_needy_image_build_second() {
  let dir = tempfile::tempdir().unwrap();

  let error = LinuxFirecracker::new(dir.path()).check().unwrap_err();

  assert_eq!(
    error.to_string(),
    format!(
      "actor image is missing {}; run `needy image pull --driver linux-firecracker`, or `needy image build` in a needy checkout",
      dir.path().join("vmlinux").display()
    )
  );
}

// ROOT CAUSE:
//
// If a VM image directory was written before `needy image` (with the
// `base-abi` file and no manifest), its base ABI could not be read.
//
// The manifest replaces `base-abi`, without compatibility: such a directory
// is missing its manifest, and the message names the commands that write
// one.
#[test]
fn a_vm_image_without_a_manifest_names_the_commands_that_write_one() {
  let dir = tempfile::tempdir().unwrap();
  macos_vz_files(dir.path());
  std::fs::write(dir.path().join(base::FILE), "1\n").unwrap();

  let error = MacosVz::new(dir.path()).check().unwrap_err().to_string();

  assert_eq!(
    error,
    format!(
      "actor image is missing {}; run `needy image pull --driver macos-vz`, or `needy image build` in a needy checkout",
      dir.path().join(manifest::FILE).display()
    )
  );
}

#[test]
fn a_base_vm_image_of_another_base_abi_names_needy_image_pull() {
  let dir = tempfile::tempdir().unwrap();
  macos_vz_files(dir.path());
  write_manifest(dir.path(), ActorDriver::MacosVz, 2);

  let error = MacosVz::new(dir.path()).check().unwrap_err().to_string();

  assert!(error.contains("has base ABI 2, and this needy speaks base ABI 1"), "{error}");
  assert!(
    error.contains("pull this needy's base with `needy image pull --driver macos-vz`, or rebuild it with `needy image build`"),
    "{error}"
  );
}

#[test]
fn a_vm_image_of_another_driver_is_refused() {
  let dir = tempfile::tempdir().unwrap();
  macos_vz_files(dir.path());
  write_manifest(dir.path(), ActorDriver::LinuxFirecracker, base::ABI);

  let error = MacosVz::new(dir.path()).check().unwrap_err().to_string();

  assert!(error.contains("is a linux-firecracker image, not a macos-vz one"), "{error}");
}

#[test]
fn each_vm_driver_has_its_base_in_its_own_directory() {
  let images = Path::new("/home/user/.local/share/needy/images");

  assert_eq!(base_dir(images, ActorDriver::MacosVz), images.join("macos-vz").join("base"));
  assert_eq!(base_dir(images, ActorDriver::LinuxFirecracker), images.join("linux-firecracker").join("base"));
  assert_ne!(base_dir(images, ActorDriver::MacosVz), dir_of(images, ActorDriver::MacosVz, "base"));
}
