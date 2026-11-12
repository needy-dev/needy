use needy_core_actor::image::manifest::Manifest;
use needy_core_actor::image::vm;

use super::*;

#[test]
fn each_vm_image_is_listed_with_what_its_manifest_says() {
  let images = tempfile::tempdir().unwrap();
  let base = vm::base_dir(images.path(), ActorDriver::MacosVz);
  std::fs::create_dir_all(&base).unwrap();
  std::fs::write(base.join("rootfs.ext4"), vec![1u8; 8192]).unwrap();
  let mut pulled = Manifest::made(
    ActorDriver::MacosVz,
    1,
    manifest::Source::Release {
      tag: "v0.0.1".into(),
      url: "https://example.com/needy-actor-macos-vz-aarch64.tar.gz".into(),
    },
  );
  pulled.version = "0.0.1".into();
  manifest::write(&base, &pulled).unwrap();
  let custom = vm::dir_of(images.path(), ActorDriver::MacosVz, "needy-custom-jq");
  std::fs::create_dir_all(&custom).unwrap();
  let source = manifest::Source::Oci {
    reference: "needy-custom-jq:latest".into(),
    id: "sha256:0123".into(),
    platform: "linux/arm64".into(),
  };
  manifest::write(&custom, &Manifest::made(ActorDriver::MacosVz, 1, source)).unwrap();
  // A pull in progress, and a directory written before manifests.
  std::fs::create_dir_all(images.path().join("macos-vz").join(".needy-pull-0")).unwrap();
  let old = images.path().join("linux-firecracker").join("old");
  std::fs::create_dir_all(&old).unwrap();

  let rows = vm_images(images.path());

  let cells: Vec<Vec<String>> = rows.iter().map(|row| row.cells()[..5].to_vec()).collect();
  assert_eq!(
    cells,
    [
      ["linux-firecracker", "old", "-", "-", "no manifest.json"],
      ["macos-vz", "base", "0.0.1", "1", "release v0.0.1"],
      [
        "macos-vz",
        "needy-custom-jq:latest",
        env!("CARGO_PKG_VERSION"),
        "1",
        "built from needy-custom-jq:latest (sha256:0123)"
      ],
    ]
  );
  assert_eq!(rows[1].path.as_deref(), Some(base.as_path()));
  assert!(rows[1].size >= 8192, "{}", rows[1].size);
}

#[test]
fn sizes_are_shown_in_binary_units() {
  assert_eq!(size(512), "512 B");
  assert_eq!(size(1536), "1.5 KiB");
  assert_eq!(size(600 << 20), "600.0 MiB");
  assert_eq!(size(3 << 30), "3.0 GiB");
}
