use super::*;

fn pulled() -> Manifest {
  Manifest {
    version: "0.0.1".into(),
    driver: ActorDriver::MacosVz,
    arch: "aarch64".into(),
    base_abi: 1,
    source: Some(Source::Release {
      tag: "v0.0.1".into(),
      url: "https://example.com/needy-actor-macos-vz-aarch64.tar.gz".into(),
    }),
    sha256: Some("0".repeat(64)),
    base: None,
  }
}

#[test]
fn a_manifest_is_read_back_as_written() {
  let dir = tempfile::tempdir().unwrap();

  write(dir.path(), &pulled()).unwrap();

  assert_eq!(read(dir.path()).unwrap(), Some(pulled()));
}

#[test]
fn a_directory_without_a_manifest_has_none() {
  let dir = tempfile::tempdir().unwrap();

  assert_eq!(read(dir.path()).unwrap(), None);
}

// The release workflow writes this shape into each VM image archive with
// `jq` (`.github/workflows/release.yml`), and `needy image pull` reads it:
// keep the two in step.
#[test]
fn the_manifest_of_a_release_archive_has_no_source_or_sha256() {
  let dir = tempfile::tempdir().unwrap();
  std::fs::write(dir.path().join(FILE), r#"{"version":"0.0.1","driver":"linux-firecracker","arch":"x86_64","base_abi":1}"#).unwrap();

  let manifest = read(dir.path()).unwrap().unwrap();

  assert_eq!(manifest.driver, ActorDriver::LinuxFirecracker);
  assert_eq!((manifest.source, manifest.sha256), (None, None));
}

// A manifest written before needy recorded the base (needy 0.0.17 and
// earlier, release archives and `needy image build`) has no `base`, and is
// read as before.
#[test]
fn a_manifest_without_a_base_is_read_with_none() {
  let dir = tempfile::tempdir().unwrap();
  std::fs::write(
    dir.path().join(FILE),
    r#"{"version":"0.0.17","driver":"macos-vz","arch":"aarch64","base_abi":1,"source":{"kind":"build"}}"#,
  )
  .unwrap();

  let manifest = read(dir.path()).unwrap().unwrap();

  assert_eq!(manifest.base, None);
  assert_eq!(manifest.source, Some(Source::Build { recipe: None }));
}

// The release workflow's "Write the image's manifest" step writes `base`
// with `jq` from the `actor-base` image it built: the published name, the
// digest when it pushed the base, and its DiffIDs.
#[test]
fn the_manifest_of_a_release_archive_records_its_base() {
  let dir = tempfile::tempdir().unwrap();
  let digest = format!("sha256:{}", "a".repeat(64));
  let (root, userland) = (format!("sha256:{}", "1".repeat(64)), format!("sha256:{}", "2".repeat(64)));
  let json = serde_json::json!({
    "version": "0.0.18",
    "driver": "linux-firecracker",
    "arch": "x86_64",
    "base_abi": 1,
    "base": {
      "image": "ghcr.io/needy-dev/needy/actor-base:0.0.18-ubuntu-24.04",
      "digest": digest,
      "diff_ids": [root, userland],
    },
  });
  std::fs::write(dir.path().join(FILE), json.to_string()).unwrap();

  let manifest = read(dir.path()).unwrap().unwrap();

  assert_eq!(
    manifest.base,
    Some(Base {
      image: "ghcr.io/needy-dev/needy/actor-base:0.0.18-ubuntu-24.04".into(),
      digest: Some(digest),
      diff_ids: vec![root, userland],
    })
  );
  assert_eq!(serde_json::to_value(&manifest).unwrap(), json);
}

#[test]
fn a_base_without_a_digest_is_written_without_one() {
  let base = Base {
    image: "needy-actor-base:latest".into(),
    digest: None,
    diff_ids: vec!["sha256:01".into()],
  };

  let json = serde_json::to_value(&base).unwrap();

  assert_eq!(json, serde_json::json!({"image": "needy-actor-base:latest", "diff_ids": ["sha256:01"]}));
}

#[test]
fn the_manifest_names_its_fields_and_sources_in_json() {
  let built = Manifest {
    source: Some(Source::Oci {
      reference: "needy-custom-jq:latest".into(),
      id: "sha256:0123".into(),
      platform: "linux/arm64".into(),
    }),
    sha256: None,
    ..pulled()
  };

  let json: serde_json::Value = serde_json::to_value(&built).unwrap();

  assert_eq!(
    json,
    serde_json::json!({
      "version": "0.0.1",
      "driver": "macos-vz",
      "arch": "aarch64",
      "base_abi": 1,
      "source": {"kind": "oci", "reference": "needy-custom-jq:latest", "id": "sha256:0123", "platform": "linux/arm64"},
    })
  );
  assert_eq!(serde_json::to_value(Source::Build { recipe: None }).unwrap(), serde_json::json!({"kind": "build"}));
}

#[test]
fn a_manifest_with_an_unknown_field_is_refused() {
  let dir = tempfile::tempdir().unwrap();
  std::fs::write(dir.path().join(FILE), r#"{"version":"0.0.1","driver":"macos-vz","arch":"aarch64","base_abi":1,"kernel":"x"}"#).unwrap();

  let error = read(dir.path()).unwrap_err().to_string();

  assert!(error.contains("is not a VM image manifest: unknown field `kernel`"), "{error}");
}
