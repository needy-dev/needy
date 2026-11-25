use std::path::Path;

use oci_registry::testing::{Auth, TestRegistry};

use super::*;

fn header(kind: tar::EntryType, mode: u32) -> tar::Header {
  let mut header = tar::Header::new_gnu();
  header.set_entry_type(kind);
  header.set_mode(mode);
  header.set_uid(0);
  header.set_gid(0);
  header.set_mtime(0);
  header.set_size(0);
  header
}

/// An OCI layer with a file, a whiteout and an opaque directory.
fn layer_with_deletions() -> Vec<u8> {
  let mut tar = tar::Builder::new(Vec::new());
  for dir in ["etc", "opt", "opt/tool"] {
    tar.append_data(&mut header(tar::EntryType::Directory, 0o755), dir, std::io::empty()).unwrap();
  }
  let content = b"registry\n";
  let mut file = header(tar::EntryType::Regular, 0o644);
  file.set_size(content.len() as u64);
  tar.append_data(&mut file, "etc/hostname", content.as_slice()).unwrap();
  // `etc/motd` deleted, and everything below `opt/tool` hidden.
  tar.append_data(&mut header(tar::EntryType::Regular, 0o644), "etc/.wh.motd", std::io::empty()).unwrap();
  tar.append_data(&mut header(tar::EntryType::Regular, 0o644), "opt/tool/.wh..wh..opq", std::io::empty()).unwrap();
  tar.into_inner().unwrap()
}

fn store(dir: &Path) -> Store {
  Store::new(dir.join("cache"), dir.join("data"))
}

fn entries(store: &Store, digest: &Digest) -> Vec<String> {
  let file = std::fs::File::open(store.layer(digest).unwrap()).unwrap();
  let mut archive = tar::Archive::new(file);
  archive.entries().unwrap().map(|entry| entry.unwrap().path().unwrap().display().to_string()).collect()
}

fn step(diff_id: Digest, key: &str) -> Step {
  Step {
    diff_id,
    key: Digest::of(key.as_bytes()),
    parent: Some(Digest::of(b"parent")),
    root: Digest::of(b"root"),
    feature: Some(Feature {
      reference: "ghcr.io/devcontainers/features/node:1".into(),
      digest: Digest::of(b"feature"),
    }),
  }
}

fn access() -> Access {
  Access::standard().with_cert_roots(Vec::new()).with_docker_config("/nonexistent").without_proxy()
}

fn registry_for(test: &TestRegistry) -> Registry {
  Registry::new(Repository::parse(&test.repository("needy/layers")).unwrap(), access())
}

#[test]
fn a_repository_is_a_registry_host_and_a_path_over_https_unless_it_says_http() {
  let https = Repository::parse("registry.example.com/needy/layers").unwrap();
  assert_eq!((https.registry(), https.path(), https.plain_http()), ("registry.example.com", "needy/layers", false));
  assert_eq!(https.to_string(), "registry.example.com/needy/layers");
  assert_eq!(Repository::parse("https://registry.example.com/needy/layers/").unwrap(), https);

  let http = Repository::parse("http://registry.example.com:5000/layers").unwrap();
  assert_eq!((http.registry(), http.path(), http.plain_http()), ("registry.example.com:5000", "layers", true));
  assert_eq!(http.to_string(), "http://registry.example.com:5000/layers");

  let key = Digest::of(b"key");
  assert_eq!(http.reference(&key), format!("registry.example.com:5000/layers:key-{}", key.hex()));

  for bad in [
    "",
    "registry.example.com",
    "needy/layers",
    "ftp://registry.example.com/layers",
    "registry.example.com/needy/layers:latest",
    "registry.example.com/needy/layers@sha256:0000000000000000000000000000000000000000000000000000000000000000",
    "registry.example.com/Needy/Layers",
    "registry.example.com/needy//layers",
  ] {
    assert!(matches!(Repository::parse(bad), Err(Error::Repository { .. })), "{bad:?} parsed");
  }
}

#[tokio::test]
async fn a_pushed_step_is_an_ordinary_oci_image_that_pulls_into_a_fresh_store_as_the_same_layer() {
  // Over TLS with a private CA in `certs.d` and Harbor-shaped credentials,
  // as a team registry is.
  let test = TestRegistry::start_with(
    true,
    Auth::Token {
      username: "robot$needy".into(),
      password: "robot-secret".into(),
    },
  )
  .await;
  let config = tempfile::tempdir().unwrap();
  let auth = {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode("robot$needy:robot-secret")
  };
  std::fs::write(config.path().join("config.json"), serde_json::json!({ "auths": { test.host(): { "auth": auth } } }).to_string()).unwrap();
  let certs_d = tempfile::tempdir().unwrap();
  let access =
    Access::standard().with_cert_roots(vec![test.write_certs_d(certs_d.path())]).with_docker_config(config.path()).without_proxy();
  let client = Registry::new(Repository::parse(&test.repository("needy/layers")).unwrap(), access);

  let pusher_dir = tempfile::tempdir().unwrap();
  let pusher = store(pusher_dir.path());
  let diff_id = pusher.put(layer_with_deletions().as_slice()).unwrap();
  let step = step(diff_id, "step");
  let key = step.key;

  assert_eq!(client.lookup(&key).await.unwrap(), None, "nothing is pushed yet");
  let pushed = client.push(&pusher, &step).await.unwrap();
  let tag = format!("key-{}", key.hex());
  assert_eq!(pushed.reference, format!("{}/needy/layers:{tag}", test.host()));
  assert_eq!(test.tags("needy/layers"), std::slice::from_ref(&tag));

  // What a standard tool reads: an OCI image manifest, a standard config
  // and one gzip layer whose DiffID the config names.
  let (media_type, manifest_bytes) = test.manifest("needy/layers", &tag).unwrap();
  assert_eq!(media_type, "application/vnd.oci.image.manifest.v1+json");
  assert_eq!(Digest::of(&manifest_bytes), pushed.manifest);
  let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();
  assert_eq!(manifest["schemaVersion"], 2);
  assert_eq!(manifest["mediaType"], "application/vnd.oci.image.manifest.v1+json");
  assert_eq!(manifest["config"]["mediaType"], "application/vnd.oci.image.config.v1+json");
  assert_eq!(manifest["layers"].as_array().unwrap().len(), 1);
  assert_eq!(manifest["layers"][0]["mediaType"], "application/vnd.oci.image.layer.v1.tar+gzip");
  assert_eq!(manifest["layers"][0]["digest"], pushed.blob.to_string());
  assert_eq!(manifest["annotations"]["dev.needy.image.step.key"], key.to_string());
  assert_eq!(manifest["annotations"]["dev.needy.image.step.parent"], Digest::of(b"parent").to_string());
  assert_eq!(manifest["annotations"]["dev.needy.image.root"], Digest::of(b"root").to_string());
  assert_eq!(manifest["annotations"]["dev.needy.image.feature.ref"], "ghcr.io/devcontainers/features/node:1");
  assert_eq!(manifest["annotations"]["dev.needy.image.feature.digest"], Digest::of(b"feature").to_string());
  let config = test.blob(manifest["config"]["digest"].as_str().unwrap()).unwrap();
  let config: serde_json::Value = serde_json::from_slice(&config).unwrap();
  assert_eq!(config["os"], "linux");
  assert_eq!(config["architecture"], Arch::default().to_string());
  assert_eq!(config["rootfs"], serde_json::json!({ "type": "layers", "diff_ids": [diff_id.to_string()] }));
  let blob = test.blob(&pushed.blob.to_string()).unwrap();
  let mut decompressed = Vec::new();
  std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(blob.as_slice()), &mut decompressed).unwrap();
  assert_eq!(Digest::of(&decompressed), diff_id, "the gzip blob decompresses to the DiffID");

  let found = client.lookup(&key).await.unwrap().expect("the pushed step is found");
  assert_eq!(found.step, step);
  assert_eq!(found.manifest, pushed.manifest);

  let puller_dir = tempfile::tempdir().unwrap();
  let puller = store(puller_dir.path());
  assert!(!puller.has_layer(&diff_id));
  assert_eq!(client.pull(&puller, &found).await.unwrap(), diff_id);
  assert_eq!(std::fs::read(puller.layer(&diff_id).unwrap()).unwrap(), std::fs::read(pusher.layer(&diff_id).unwrap()).unwrap());
  let pulled_entries = entries(&puller, &diff_id);
  for expected in ["etc/hostname", "etc/.wh.motd", "opt/tool/.wh..wh..opq"] {
    assert!(pulled_entries.iter().any(|entry| entry == expected), "{expected} missing from {pulled_entries:?}");
  }
}

#[tokio::test]
async fn a_step_whose_config_names_another_diff_id_is_refused_and_not_kept() {
  let test = TestRegistry::start().await;
  let client = registry_for(&test);
  let pusher_dir = tempfile::tempdir().unwrap();
  let pusher = store(pusher_dir.path());
  let diff_id = pusher.put(layer_with_deletions().as_slice()).unwrap();
  let honest = step(diff_id, "step");
  let pushed = client.push(&pusher, &honest).await.unwrap();

  // Another key's image: the honest blob, and a config that claims another
  // DiffID for it.
  let claimed = Digest::of(b"another layer");
  let tampered_key = Digest::of(b"tampered");
  let config = serde_json::to_vec(&image_config(&claimed).unwrap()).unwrap();
  let config_digest = test.put_blob(&config);
  let mut annotations = honest.annotations();
  annotations.insert(annotation::KEY.into(), tampered_key.to_string());
  let manifest = serde_json::json!({
    "schemaVersion": 2,
    "mediaType": OCI_IMAGE_MEDIA_TYPE,
    "config": { "mediaType": IMAGE_CONFIG_MEDIA_TYPE, "digest": config_digest, "size": config.len() },
    "layers": [{ "mediaType": IMAGE_LAYER_GZIP_MEDIA_TYPE, "digest": pushed.blob.to_string(), "size": pushed.size }],
    "annotations": annotations,
  });
  test.put_manifest("needy/layers", &Repository::tag(&tampered_key), OCI_IMAGE_MEDIA_TYPE, manifest.to_string().as_bytes());

  let found = client.lookup(&tampered_key).await.unwrap().expect("the tampered image is found");
  let puller_dir = tempfile::tempdir().unwrap();
  let puller = store(puller_dir.path());
  let err = client.pull(&puller, &found).await.unwrap_err();
  assert!(matches!(&err, Error::DiffId { expected, actual, .. } if *expected == claimed && *actual == diff_id), "{err}");
  assert!(!puller.has_layer(&diff_id), "a refused layer is not kept");
  assert!(!puller.has_layer(&claimed));
}

#[tokio::test]
async fn a_blob_the_registry_serves_tampered_is_refused_and_not_kept() {
  let test = TestRegistry::start().await;
  let client = registry_for(&test);
  let pusher_dir = tempfile::tempdir().unwrap();
  let pusher = store(pusher_dir.path());
  let diff_id = pusher.put(layer_with_deletions().as_slice()).unwrap();
  let step = step(diff_id, "step");
  let pushed = client.push(&pusher, &step).await.unwrap();
  let mut tampered = test.blob(&pushed.blob.to_string()).unwrap();
  let last = tampered.len() - 1;
  tampered[last] ^= 0xff;
  test.tamper_blob(&pushed.blob.to_string(), &tampered);

  let found = client.lookup(&step.key).await.unwrap().unwrap();
  let puller_dir = tempfile::tempdir().unwrap();
  let puller = store(puller_dir.path());
  let err = client.pull(&puller, &found).await.unwrap_err();
  assert!(matches!(err, Error::Registry { .. }), "{err}");
  assert!(!puller.has_layer(&diff_id));
}

#[tokio::test]
async fn a_tag_that_is_not_a_needy_step_image_is_refused() {
  let test = TestRegistry::start().await;
  let client = registry_for(&test);
  let key = Digest::of(b"foreign");
  let config = b"{}";
  let digest = test.put_blob(config);
  let manifest = serde_json::json!({
    "schemaVersion": 2,
    "mediaType": OCI_IMAGE_MEDIA_TYPE,
    "config": { "mediaType": "application/vnd.example.config.v1+json", "digest": digest, "size": config.len() },
    "layers": [],
  });
  test.put_manifest("needy/layers", &Repository::tag(&key), OCI_IMAGE_MEDIA_TYPE, manifest.to_string().as_bytes());

  let err = client.lookup(&key).await.unwrap_err();
  assert!(matches!(err, Error::Invalid { .. }), "{err}");
}

// zstd:chunked layers (Podman, Buildah) are many zstd frames with skippable
// frames between them; a layer is all of its frames.
#[test]
fn a_zstd_blob_decompresses_every_frame_and_skips_skippable_frames() {
  let dir = tempfile::tempdir().unwrap();
  let mut blob = ruzstd::encoding::compress_to_vec(&b"first frame, "[..], ruzstd::encoding::CompressionLevel::Fastest);
  // A skippable frame: magic 0x184D2A50..=0x184D2A5F, a 4-byte
  // little-endian length, and that many bytes (RFC 8878, 3.1.2).
  blob.extend(0x184D_2A5Eu32.to_le_bytes());
  blob.extend(5u32.to_le_bytes());
  blob.extend(b"skip!");
  blob.extend(ruzstd::encoding::compress_to_vec(&b"second frame"[..], ruzstd::encoding::CompressionLevel::Fastest));
  let path = dir.path().join("blob");
  std::fs::write(&path, &blob).unwrap();

  let (uncompressed, digest) = decompress(&path, Compression::Zstd).unwrap();

  assert_eq!(std::fs::read(uncompressed.path()).unwrap(), b"first frame, second frame");
  assert_eq!(digest, Digest::of(b"first frame, second frame"));
}
