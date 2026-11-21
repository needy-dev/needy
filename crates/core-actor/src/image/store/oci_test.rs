use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;

use super::*;
use crate::image::base;
use crate::image::store::Store;

/// Helper to build an uncompressed layer tar in memory.
struct LayerBuilder(tar::Builder<Vec<u8>>);

impl LayerBuilder {
  fn new() -> Self {
    Self(tar::Builder::new(Vec::new()))
  }

  fn dir(mut self, path: &str) -> Self {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Directory);
    header.set_mode(0o755);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_size(0);
    self.0.append_data(&mut header, path, std::io::empty()).unwrap();
    self
  }

  fn file(mut self, path: &str, mode: u32, content: &[u8]) -> Self {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_size(content.len() as u64);
    self.0.append_data(&mut header, path, content).unwrap();
    self
  }

  fn symlink(mut self, path: &str, target: &str) -> Self {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_mode(0o777);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_size(0);
    self.0.append_link(&mut header, path, target).unwrap();
    self
  }

  fn bytes(self) -> Vec<u8> {
    self.0.into_inner().unwrap()
  }
}

fn base_config(diff_ids: &[Digest]) -> serde_json::Value {
  serde_json::json!({
    "architecture": "arm64",
    "os": "linux",
    "config": {
      "Env": ["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"],
      "Entrypoint": ["/.needy/init"],
      "Cmd": ["sh"],
      "Labels": {
        base::LABEL: base::ABI.to_string(),
        "example.keep": "yes"
      }
    },
    "rootfs": {
      "type": "layers",
      "diff_ids": diff_ids.iter().map(ToString::to_string).collect::<Vec<_>>()
    },
    "history": diff_ids.iter().map(|_| serde_json::json!({ "created_by": "base" })).collect::<Vec<_>>()
  })
}

/// Writes a classic Docker-save archive (`manifest.json`, `<hex>.json`, `<id>/layer.tar`).
fn write_docker_save(path: &Path, layers: &[Vec<u8>]) {
  let diff_ids: Vec<Digest> = layers.iter().map(|b| Digest::of(b)).collect();
  let config_bytes = serde_json::to_vec(&base_config(&diff_ids)).unwrap();
  let config_hex = Digest::of(&config_bytes).hex();
  let config_name = format!("{config_hex}.json");

  let mut layer_names = Vec::new();
  let mut builder = tar::Builder::new(File::create(path).unwrap());
  append_bytes(&mut builder, &config_name, &config_bytes).unwrap();
  for (idx, layer) in layers.iter().enumerate() {
    let name = format!("layer{idx}/layer.tar");
    append_bytes(&mut builder, &name, layer).unwrap();
    layer_names.push(name);
  }
  let manifest = serde_json::json!([{
    "Config": config_name,
    "RepoTags": ["needy-actor:base"],
    "Layers": layer_names
  }]);
  append_bytes(&mut builder, "manifest.json", &serde_json::to_vec(&manifest).unwrap()).unwrap();
  builder.finish().unwrap();
}

/// Writes an OCI Image Layout archive (`oci-layout`, `index.json`, `blobs/sha256/...`)
/// with gzip-compressed layers.
fn write_oci_layout_gzip(path: &Path, layers: &[Vec<u8>]) {
  let diff_ids: Vec<Digest> = layers.iter().map(|b| Digest::of(b)).collect();
  let config_bytes = serde_json::to_vec(&base_config(&diff_ids)).unwrap();
  let config_digest = Digest::of(&config_bytes);

  let mut gz_layers = Vec::new();
  for layer in layers {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(layer).unwrap();
    let gz = enc.finish().unwrap();
    let gz_digest = Digest::of(&gz);
    gz_layers.push((gz_digest, gz));
  }

  let manifest = serde_json::json!({
    "schemaVersion": 2,
    "mediaType": MEDIA_TYPE_OCI_MANIFEST,
    "config": {
      "mediaType": MEDIA_TYPE_OCI_CONFIG,
      "digest": config_digest.to_string(),
      "size": config_bytes.len()
    },
    "layers": gz_layers.iter().map(|(d, b)| serde_json::json!({
      "mediaType": MEDIA_TYPE_OCI_LAYER_TAR_GZIP,
      "digest": d.to_string(),
      "size": b.len()
    })).collect::<Vec<_>>()
  });
  let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
  let manifest_digest = Digest::of(&manifest_bytes);

  let index = serde_json::json!({
    "schemaVersion": 2,
    "mediaType": MEDIA_TYPE_OCI_INDEX,
    "manifests": [{
      "mediaType": MEDIA_TYPE_OCI_MANIFEST,
      "digest": manifest_digest.to_string(),
      "size": manifest_bytes.len()
    }]
  });

  let mut builder = tar::Builder::new(File::create(path).unwrap());
  append_bytes(&mut builder, "oci-layout", br#"{"imageLayoutVersion":"1.0.0"}"#).unwrap();
  append_bytes(&mut builder, "index.json", &serde_json::to_vec(&index).unwrap()).unwrap();
  append_bytes(&mut builder, &blob_path(&config_digest), &config_bytes).unwrap();
  append_bytes(&mut builder, &blob_path(&manifest_digest), &manifest_bytes).unwrap();
  for (d, b) in &gz_layers {
    append_bytes(&mut builder, &blob_path(d), b).unwrap();
  }
  builder.finish().unwrap();
}

/// Reads all regular file entries of a tar archive into a map by path.
fn read_tar_entries(path: &Path) -> BTreeMap<String, Vec<u8>> {
  let mut archive = tar::Archive::new(File::open(path).unwrap());
  let mut map = BTreeMap::new();
  for entry in archive.entries().unwrap() {
    let mut entry = entry.unwrap();
    let name = entry.path().unwrap().to_string_lossy().into_owned();
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes).unwrap();
    map.insert(name, bytes);
  }
  map
}

/// Reads a layer tar's entries (path -> (entry_type, content_or_link)).
fn read_layer_tree(layer_bytes: &[u8]) -> BTreeMap<String, (tar::EntryType, Vec<u8>)> {
  let mut archive = tar::Archive::new(layer_bytes);
  let mut map = BTreeMap::new();
  for entry in archive.entries().unwrap() {
    let mut entry = entry.unwrap();
    let kind = entry.header().entry_type();
    let name = entry.path().unwrap().to_string_lossy().into_owned();
    let payload = if kind == tar::EntryType::Symlink {
      entry.link_name().unwrap().unwrap().to_string_lossy().as_bytes().to_vec()
    } else {
      let mut bytes = Vec::new();
      entry.read_to_end(&mut bytes).unwrap();
      bytes
    };
    map.insert(name, (kind, payload));
  }
  map
}

#[test]
fn snapshot_tag_is_deterministic_and_needy_owned() {
  let d = Digest::of(b"snapshot-stack");
  let tag = snapshot_tag(&d);
  assert!(tag.starts_with("needy-snapshot:"), "{tag}");
  assert_eq!(tag, format!("needy-snapshot:{}", d.hex()));
  assert_eq!(tag, snapshot_tag(&d));
  assert_ne!(tag, snapshot_tag(&Digest::of(b"other-stack")));
}

#[test]
fn materialize_container_assembles_valid_archive_from_docker_save_base() {
  let dir = tempfile::tempdir().unwrap();
  let store = Store::new(dir.path().join("cache"), dir.path().join("data"));

  let base_layer =
    LayerBuilder::new().dir(".needy").file(".needy/proxy", 0o755, b"PROXY-STUB").dir("etc").file("etc/base", 0o644, b"base\n").bytes();
  let base_tar = dir.path().join("base.tar");
  write_docker_save(&base_tar, std::slice::from_ref(&base_layer));

  let layer1 = store.put(LayerBuilder::new().dir("opt").file("opt/one", 0o644, b"1").bytes().as_slice()).unwrap();
  let layer2 = store.put(LayerBuilder::new().dir("opt").file("opt/two", 0o644, b"2").bytes().as_slice()).unwrap();
  let snapshot = store.snapshot(&[layer1, layer2]).unwrap();
  let tag = snapshot_tag(&snapshot.digest);

  let out = store.materialize_container(&snapshot, &base_tar, &tag).unwrap();
  assert!(out.starts_with(dir.path().join("cache").join("oci")));

  let entries = read_tar_entries(&out);
  assert!(entries.contains_key("oci-layout"));
  assert!(entries.contains_key("index.json"));
  let manifest_entries: Vec<serde_json::Value> = serde_json::from_slice(&entries["manifest.json"]).unwrap();
  assert_eq!(manifest_entries.len(), 1);
  assert_eq!(manifest_entries[0]["RepoTags"], serde_json::json!([tag]));

  let layers_list: Vec<String> = serde_json::from_value(manifest_entries[0]["Layers"].clone()).unwrap();
  assert_eq!(layers_list.len(), 3, "base + 2 snapshot layers (no shells touched, so no shim layer)");

  let config_path = manifest_entries[0]["Config"].as_str().unwrap();
  let config: serde_json::Value = serde_json::from_slice(&entries[config_path]).unwrap();
  assert_eq!(config["config"]["Labels"][base::LABEL], serde_json::json!(base::ABI.to_string()));
  assert_eq!(config["config"]["Labels"]["example.keep"], serde_json::json!("yes"));
  assert_eq!(config["config"]["Entrypoint"], serde_json::json!(["/.needy/init"]));
  assert_eq!(config["architecture"], serde_json::json!("arm64"));
  assert_eq!(config["os"], serde_json::json!("linux"));

  let diff_ids: Vec<String> = serde_json::from_value(config["rootfs"]["diff_ids"].clone()).unwrap();
  assert_eq!(diff_ids.len(), 3);
  for (layer_path, diff_id) in layers_list.iter().zip(&diff_ids) {
    let tar_bytes = &entries[layer_path];
    assert_eq!(Digest::of(tar_bytes).to_string(), *diff_id);
  }
  assert_eq!(diff_ids[1], layer1.to_string());
  assert_eq!(diff_ids[2], layer2.to_string());
}

#[test]
fn materialize_container_accepts_oci_layout_base_with_gzip_layers() {
  let dir = tempfile::tempdir().unwrap();
  let store = Store::new(dir.path().join("cache"), dir.path().join("data"));

  let base_layer = LayerBuilder::new().dir(".needy").file(".needy/proxy", 0o755, b"PROXY-STUB").bytes();
  let base_tar = dir.path().join("base-oci.tar");
  write_oci_layout_gzip(&base_tar, std::slice::from_ref(&base_layer));

  let layer1 = store.put(LayerBuilder::new().dir("usr/bin").file("usr/bin/bash", 0o755, b"NEW-BASH").bytes().as_slice()).unwrap();
  let snapshot = store.snapshot(&[layer1]).unwrap();
  let tag = snapshot_tag(&snapshot.digest);

  let out = store.materialize_container(&snapshot, &base_tar, &tag).unwrap();
  let entries = read_tar_entries(&out);
  let manifest_entries: Vec<serde_json::Value> = serde_json::from_slice(&entries["manifest.json"]).unwrap();
  let layers_list: Vec<String> = serde_json::from_value(manifest_entries[0]["Layers"].clone()).unwrap();
  // Base gzip layer + snapshot layer + synthesized shim layer (proxy found inside gzip base layer!)
  assert_eq!(layers_list.len(), 3);

  let shim_tree = read_layer_tree(&entries[&layers_list[2]]);
  assert_eq!(shim_tree[".needy/real/usr/bin/bash"].1, b"NEW-BASH");
  assert_eq!(shim_tree["usr/bin/bash"].1, b"PROXY-STUB");
}

#[test]
fn materialize_container_shims_replaced_shells_and_skips_when_not_needed() {
  let dir = tempfile::tempdir().unwrap();
  let store = Store::new(dir.path().join("cache"), dir.path().join("data"));

  let base_layer = LayerBuilder::new().dir(".needy").file(".needy/proxy", 0o755, b"PROXY-BIN").bytes();
  let base_tar = dir.path().join("base.tar");
  write_docker_save(&base_tar, std::slice::from_ref(&base_layer));

  // Case 1: snapshot replaces `/bin/sh`, `/usr/bin/bash`, and symlinks `/usr/local/bin/zsh -> /opt/zsh/bin/zsh`.
  // Also adds `/bin/dash -> bash` (symlink within shimmed dirs: should NOT be shimmed).
  let l1 = store
    .put(
      LayerBuilder::new()
        .dir("bin")
        .file("bin/sh", 0o755, b"REAL-SH")
        .symlink("bin/dash", "bash")
        .dir("usr/bin")
        .file("usr/bin/bash", 0o755, b"REAL-BASH")
        .dir("usr/local/bin")
        .symlink("usr/local/bin/zsh", "/opt/zsh/bin/zsh")
        .bytes()
        .as_slice(),
    )
    .unwrap();
  let snap1 = store.snapshot(&[l1]).unwrap();
  let out1 = store.materialize_container(&snap1, &base_tar, &snapshot_tag(&snap1.digest)).unwrap();

  let entries = read_tar_entries(&out1);
  let manifest: Vec<serde_json::Value> = serde_json::from_slice(&entries["manifest.json"]).unwrap();
  let layers_list: Vec<String> = serde_json::from_value(manifest[0]["Layers"].clone()).unwrap();
  assert_eq!(layers_list.len(), 3, "base + l1 + shim layer");

  let config_path = manifest[0]["Config"].as_str().unwrap();
  let config: serde_json::Value = serde_json::from_slice(&entries[config_path]).unwrap();
  assert_eq!(config["config"]["Labels"][base::LABEL], serde_json::json!(base::ABI.to_string()));
  let diff_ids: Vec<String> = serde_json::from_value(config["rootfs"]["diff_ids"].clone()).unwrap();
  assert_eq!(diff_ids.len(), 3);
  assert_eq!(Digest::of(&entries[&layers_list[2]]).to_string(), diff_ids[2]);

  let shim_tree = read_layer_tree(&entries[&layers_list[2]]);
  assert_eq!(shim_tree[".needy/real/bin/sh"].1, b"REAL-SH");
  assert_eq!(shim_tree["bin/sh"].1, b"PROXY-BIN");
  assert_eq!(shim_tree[".needy/real/usr/bin/bash"].1, b"REAL-BASH");
  assert_eq!(shim_tree["usr/bin/bash"].1, b"PROXY-BIN");
  assert_eq!(shim_tree[".needy/real/usr/local/bin/zsh"], (tar::EntryType::Symlink, b"/opt/zsh/bin/zsh".to_vec()));
  assert_eq!(shim_tree["usr/local/bin/zsh"].1, b"PROXY-BIN");
  assert!(!shim_tree.contains_key("bin/dash"), "symlink to another shimmed dir stays a symlink");

  // Case 2: snapshot puts a shell that is ALREADY the proxy binary -> no redundant shim layer.
  let l2 = store.put(LayerBuilder::new().dir("bin").file("bin/sh", 0o755, b"PROXY-BIN").bytes().as_slice()).unwrap();
  let snap2 = store.snapshot(&[l2]).unwrap();
  let out2 = store.materialize_container(&snap2, &base_tar, &snapshot_tag(&snap2.digest)).unwrap();
  let entries2 = read_tar_entries(&out2);
  let manifest2: Vec<serde_json::Value> = serde_json::from_slice(&entries2["manifest.json"]).unwrap();
  let layers_list2: Vec<String> = serde_json::from_value(manifest2[0]["Layers"].clone()).unwrap();
  assert_eq!(layers_list2.len(), 2, "already shimmed shell needs no extra shim layer");

  // Case 3: layer adds `/bin/sh` and a higher layer whites it out -> no shim layer.
  let l3_add = store.put(LayerBuilder::new().dir("bin").file("bin/sh", 0o755, b"TEMP-SH").bytes().as_slice()).unwrap();
  let l3_del = store.put(LayerBuilder::new().dir("bin").file("bin/.wh.sh", 0, b"").bytes().as_slice()).unwrap();
  let snap3 = store.snapshot(&[l3_add, l3_del]).unwrap();
  let out3 = store.materialize_container(&snap3, &base_tar, &snapshot_tag(&snap3.digest)).unwrap();
  let entries3 = read_tar_entries(&out3);
  let manifest3: Vec<serde_json::Value> = serde_json::from_slice(&entries3["manifest.json"]).unwrap();
  let layers_list3: Vec<String> = serde_json::from_value(manifest3[0]["Layers"].clone()).unwrap();
  assert_eq!(layers_list3.len(), 3, "base + add + whiteout, no shim layer");
}

#[test]
fn extract_top_layer_and_put_from_image_export_match_direct_put() {
  let dir = tempfile::tempdir().unwrap();
  let store = Store::new(dir.path().join("cache"), dir.path().join("data"));

  let base_layer = LayerBuilder::new().dir("etc").file("etc/base", 0o644, b"base").bytes();
  let top_layer = LayerBuilder::new().dir("opt").file("opt/new", 0o644, b"committed-change\n").bytes();

  let direct = store.put(top_layer.as_slice()).unwrap();

  let docker_export = dir.path().join("docker-export.tar");
  write_docker_save(&docker_export, &[base_layer.clone(), top_layer.clone()]);
  let from_docker = store.put_from_image_export(&docker_export).unwrap();
  assert_eq!(from_docker, direct);

  let oci_export = dir.path().join("oci-export.tar");
  write_oci_layout_gzip(&oci_export, &[base_layer, top_layer]);
  let from_oci = store.put_from_image_export(&oci_export).unwrap();
  assert_eq!(from_oci, direct);
}

#[test]
fn materialize_container_caches_and_collect_prunes_unreferenced_archives() {
  let dir = tempfile::tempdir().unwrap();
  let store = Store::new(dir.path().join("cache"), dir.path().join("data"));

  let base_layer = LayerBuilder::new().dir(".needy").file(".needy/proxy", 0o755, b"PROXY").bytes();
  let base_tar = dir.path().join("base.tar");
  write_docker_save(&base_tar, std::slice::from_ref(&base_layer));

  let kept_layer = store.put(LayerBuilder::new().file("a", 0o644, b"a").bytes().as_slice()).unwrap();
  let drop_layer = store.put(LayerBuilder::new().file("b", 0o644, b"b").bytes().as_slice()).unwrap();
  let kept_snap = store.snapshot(&[kept_layer]).unwrap();
  let drop_snap = store.snapshot(&[drop_layer]).unwrap();

  let kept_oci = store.materialize_container(&kept_snap, &base_tar, &snapshot_tag(&kept_snap.digest)).unwrap();
  let mtime_before = std::fs::metadata(&kept_oci).unwrap().modified().unwrap();
  let kept_oci_again = store.materialize_container(&kept_snap, &base_tar, &snapshot_tag(&kept_snap.digest)).unwrap();
  assert_eq!(kept_oci, kept_oci_again);
  assert_eq!(std::fs::metadata(&kept_oci_again).unwrap().modified().unwrap(), mtime_before, "cache hit must not rewrite archive");

  let drop_oci = store.materialize_container(&drop_snap, &base_tar, &snapshot_tag(&drop_snap.digest)).unwrap();
  assert!(drop_oci.exists());

  let collected = store.collect(&[kept_snap.digest]).unwrap();
  assert_eq!(collected.layers, 1);
  assert!(collected.bytes > 0);
  assert!(kept_oci.exists(), "kept snapshot's OCI archive stays");
  assert!(!drop_oci.exists(), "unreferenced snapshot's OCI archive is pruned");
}

#[test]
fn materialize_container_fails_when_snapshot_layer_is_missing() {
  let dir = tempfile::tempdir().unwrap();
  let store = Store::new(dir.path().join("cache"), dir.path().join("data"));
  let base_tar = dir.path().join("base.tar");
  write_docker_save(&base_tar, &[LayerBuilder::new().bytes()]);

  let missing = Digest::of(b"missing-layer");
  let snap = super::super::Snapshot {
    digest: Digest::of(b"fake-snap"),
    layers: vec![missing],
  };
  let err = store.materialize_container(&snap, &base_tar, "needy-snapshot:test").unwrap_err();
  assert!(matches!(err, Error::Missing(d) if d == missing), "{err}");
}

#[test]
#[ignore = "requires a local Docker daemon"]
fn docker_load_accepts_materialized_archive() {
  let dir = tempfile::tempdir().unwrap();
  let store = Store::new(dir.path().join("cache"), dir.path().join("data"));

  let base_layer = LayerBuilder::new().dir(".needy").file(".needy/proxy", 0o755, b"PROXY-STUB").bytes();
  let base_tar = dir.path().join("base.tar");
  write_docker_save(&base_tar, std::slice::from_ref(&base_layer));

  let layer1 = store.put(LayerBuilder::new().dir("bin").file("bin/sh", 0o755, b"REAL-SH").bytes().as_slice()).unwrap();
  let snap = store.snapshot(&[layer1]).unwrap();
  let tag = snapshot_tag(&snap.digest);

  let out = store.materialize_container(&snap, &base_tar, &tag).unwrap();

  let load = std::process::Command::new("docker").args(["load", "-i"]).arg(&out).output().unwrap();
  assert!(
    load.status.success(),
    "docker load failed: stdout={} stderr={}",
    String::from_utf8_lossy(&load.stdout),
    String::from_utf8_lossy(&load.stderr)
  );

  let inspect = std::process::Command::new("docker").args(["image", "inspect", &tag]).output().unwrap();
  let _ = std::process::Command::new("docker").args(["rmi", "-f", &tag]).output();
  assert!(inspect.status.success(), "docker image inspect failed: {}", String::from_utf8_lossy(&inspect.stderr));
  let info: Vec<serde_json::Value> = serde_json::from_slice(&inspect.stdout).unwrap();
  assert_eq!(info[0]["Config"]["Labels"][base::LABEL], serde_json::json!(base::ABI.to_string()));
  assert_eq!(info[0]["RootFS"]["Layers"].as_array().unwrap().len(), 3);
}
