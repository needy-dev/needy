use std::path::Path;

use super::testing::TestRegistry;
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

fn config(diff_id: Digest, key: &str) -> LayerConfig {
  LayerConfig {
    diff_id,
    key: Digest::of(key.as_bytes()),
    parent: Some(Digest::of(b"parent")),
    feature: Some(Feature {
      reference: "ghcr.io/devcontainers/features/node:1".into(),
      digest: Digest::of(b"feature"),
    }),
  }
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
async fn a_pushed_layer_pulls_into_a_fresh_store_as_the_same_layer() {
  let registry = TestRegistry::start().await;
  let client = Registry::new(Repository::parse(&registry.repository("needy/layers")).unwrap());

  let pusher_dir = tempfile::tempdir().unwrap();
  let pusher = store(pusher_dir.path());
  let diff_id = pusher.put(layer_with_deletions().as_slice()).unwrap();
  let config = config(diff_id, "step");
  let key = config.key;

  assert_eq!(client.lookup(&key).await.unwrap(), None, "nothing is pushed yet");
  let pushed = client.push(&pusher, &config).await.unwrap();
  assert_eq!(pushed.reference, format!("{}/needy/layers:key-{}", registry.host(), key.hex()));
  assert_eq!(registry.tags("needy/layers"), [format!("key-{}", key.hex())]);

  let found = client.lookup(&key).await.unwrap().expect("the pushed layer is found");
  assert_eq!(found.config, config);
  assert_eq!(found.manifest, pushed.manifest);
  assert_eq!((found.blob, found.size), (pushed.blob, pushed.size));
  assert_ne!(found.blob, diff_id, "the registry holds the layer compressed");

  let puller_dir = tempfile::tempdir().unwrap();
  let puller = store(puller_dir.path());
  assert!(!puller.has_layer(&diff_id));
  let pulled = client.pull(&puller, &found).await.unwrap();
  assert_eq!(pulled, diff_id);
  assert!(puller.has_layer(&diff_id));
  assert_eq!(std::fs::read(puller.layer(&diff_id).unwrap()).unwrap(), std::fs::read(pusher.layer(&diff_id).unwrap()).unwrap());
  let pulled_entries = entries(&puller, &diff_id);
  for expected in ["etc/hostname", "etc/.wh.motd", "opt/tool/.wh..wh..opq"] {
    assert!(pulled_entries.iter().any(|entry| entry == expected), "{expected} missing from {pulled_entries:?}");
  }
}

#[tokio::test]
async fn a_layer_whose_config_names_another_diff_id_is_refused_and_not_kept() {
  let registry = TestRegistry::start().await;
  let repository = Repository::parse(&registry.repository("needy/layers")).unwrap();
  let client = Registry::new(repository);

  let pusher_dir = tempfile::tempdir().unwrap();
  let pusher = store(pusher_dir.path());
  let diff_id = pusher.put(layer_with_deletions().as_slice()).unwrap();
  let honest = config(diff_id, "step");
  client.push(&pusher, &honest).await.unwrap();

  // Another artifact for another key, whose config claims a DiffID its
  // blob does not have: the same blob, another config.
  let tampered = LayerConfig {
    diff_id: Digest::of(b"another layer"),
    key: Digest::of(b"tampered"),
    ..honest.clone()
  };
  let honest_found = client.lookup(&honest.key).await.unwrap().unwrap();
  let tagged = client.repository().tagged(&tampered.key);
  let oci = oci_client::Client::new(ClientConfig {
    protocol: ClientProtocol::Http,
    ..Default::default()
  });
  oci.store_auth_if_needed(tagged.resolve_registry(), &RegistryAuth::Anonymous).await;
  let config_bytes = serde_json::to_vec(&tampered).unwrap();
  let config_digest = Digest::of(&config_bytes);
  oci.push_blob(&tagged, &config_bytes, &config_digest.to_string()).await.unwrap();
  let manifest = OciImageManifest {
    media_type: Some(OCI_IMAGE_MEDIA_TYPE.to_string()),
    config: OciDescriptor {
      media_type: CONFIG_MEDIA_TYPE.to_string(),
      digest: config_digest.to_string(),
      size: config_bytes.len() as i64,
      ..Default::default()
    },
    layers: vec![OciDescriptor {
      media_type: IMAGE_LAYER_GZIP_MEDIA_TYPE.to_string(),
      digest: honest_found.blob.to_string(),
      size: honest_found.size as i64,
      ..Default::default()
    }],
    ..Default::default()
  };
  oci.push_manifest(&tagged, &OciManifest::Image(manifest)).await.unwrap();

  let found = client.lookup(&tampered.key).await.unwrap().expect("the tampered artifact is found");
  let puller_dir = tempfile::tempdir().unwrap();
  let puller = store(puller_dir.path());
  let err = client.pull(&puller, &found).await.unwrap_err();
  assert!(matches!(&err, Error::DiffId { expected, actual, .. } if *expected == tampered.diff_id && *actual == diff_id), "{err}");
  assert!(!puller.has_layer(&diff_id), "a refused layer is not kept");
  assert!(!puller.has_layer(&tampered.diff_id));
}

#[tokio::test]
async fn a_tag_that_is_not_a_needy_layer_is_refused() {
  let registry = TestRegistry::start().await;
  let client = Registry::new(Repository::parse(&registry.repository("needy/layers")).unwrap());
  let key = Digest::of(b"foreign");
  let tagged = client.repository().tagged(&key);
  let oci = oci_client::Client::new(ClientConfig {
    protocol: ClientProtocol::Http,
    ..Default::default()
  });
  oci.store_auth_if_needed(tagged.resolve_registry(), &RegistryAuth::Anonymous).await;
  let config = b"{}";
  oci.push_blob(&tagged, config, &Digest::of(config).to_string()).await.unwrap();
  let manifest = OciImageManifest {
    media_type: Some(OCI_IMAGE_MEDIA_TYPE.to_string()),
    config: OciDescriptor {
      media_type: "application/vnd.oci.image.config.v1+json".into(),
      digest: Digest::of(config).to_string(),
      size: config.len() as i64,
      ..Default::default()
    },
    ..Default::default()
  };
  oci.push_manifest(&tagged, &OciManifest::Image(manifest)).await.unwrap();

  let err = client.lookup(&key).await.unwrap_err();
  assert!(matches!(err, Error::Invalid { .. }), "{err}");
}
