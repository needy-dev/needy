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
    format!("actor image is missing {}; run `needy build --from needy-custom-jq`", dir.path().join("Image").display())
  );
}

#[test]
fn a_vm_image_of_another_base_abi_names_its_reference_and_the_base_to_rebuild_from() {
  let dir = tempfile::tempdir().unwrap();
  for file in ["Image", "initrd", "rootfs.ext4"] {
    std::fs::write(dir.path().join(file), "").unwrap();
  }
  std::fs::write(dir.path().join(base::FILE), "2\n").unwrap();

  let error = MacosVz::of(dir.path(), Source::Reference("needy-custom-jq".into())).check().unwrap_err().to_string();

  assert!(error.starts_with(&format!("the actor image needy-custom-jq (the VM image {}) has base ABI 2", dir.path().display())), "{error}");
  assert!(error.contains("rebuild it FROM a base image of base ABI 1"), "{error}");
  assert!(error.contains("then run `needy build --from needy-custom-jq`"), "{error}");
}

#[test]
fn the_origin_of_a_vm_image_is_read_back() {
  let dir = tempfile::tempdir().unwrap();
  let origin = Origin {
    reference: "needy-custom-jq:latest".into(),
    id: "sha256:0123".into(),
    platform: "linux/arm64".into(),
  };
  std::fs::write(dir.path().join(ORIGIN), serde_json::to_vec(&origin).unwrap()).unwrap();

  assert_eq!(super::origin(dir.path()), Some(origin));
  assert_eq!(super::origin(&dir.path().join("none")), None);
}
