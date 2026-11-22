use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use oci_spec::image::{ImageIndex, ImageManifest, MediaType, OciLayout};

use crate::archive::{Index, Kind, canonical};
use crate::collapse::collapse;
use crate::oci::{BaseArchive, assemble_on_base, blob_path, export_layers, extract_top_layer};
use crate::{Digest, Error};

/// Helper to build a tar archive entry by entry.
struct TarBuilder(tar::Builder<Vec<u8>>);

impl TarBuilder {
  fn new() -> Self {
    Self(tar::Builder::new(Vec::new()))
  }

  fn dir(mut self, path: &str) -> Self {
    let mut header = tar_header(tar::EntryType::Directory, 0o755);
    self.0.append_data(&mut header, path, std::io::empty()).unwrap();
    self
  }

  fn file(mut self, path: &str, content: &[u8]) -> Self {
    let mut header = tar_header(tar::EntryType::Regular, 0o644);
    header.set_size(content.len() as u64);
    self.0.append_data(&mut header, path, content).unwrap();
    self
  }

  #[cfg(feature = "squashfs")]
  fn symlink(mut self, path: &str, target: &str) -> Self {
    let mut header = tar_header(tar::EntryType::Symlink, 0o777);
    self.0.append_link(&mut header, path, target).unwrap();
    self
  }

  fn hardlink(mut self, path: &str, target: &str) -> Self {
    let mut header = tar_header(tar::EntryType::Link, 0o644);
    self.0.append_link(&mut header, path, target).unwrap();
    self
  }

  fn overlay_whiteout(mut self, path: &str) -> Self {
    let mut header = tar_header(tar::EntryType::Char, 0);
    header.set_device_major(0).unwrap();
    header.set_device_minor(0).unwrap();
    self.0.append_data(&mut header, path, std::io::empty()).unwrap();
    self
  }

  fn overlay_opaque(mut self, path: &str, xattr_key: &str) -> Self {
    self.0.append_pax_extensions([(xattr_key, b"y".as_slice())]).unwrap();
    self.dir(path)
  }

  fn bytes(self) -> Vec<u8> {
    self.0.into_inner().unwrap()
  }
}

fn tar_header(kind: tar::EntryType, mode: u32) -> tar::Header {
  let mut header = tar::Header::new_gnu();
  header.set_entry_type(kind);
  header.set_mode(mode);
  header.set_uid(0);
  header.set_gid(0);
  header.set_mtime(0);
  header.set_size(0);
  header
}

fn write_canonical_layer(dir: &Path, name: &str, raw: &[u8]) -> (Digest, PathBuf) {
  let mut out = Vec::new();
  canonical(raw, &mut out).unwrap();
  let digest = Digest::of(&out);
  let path = dir.join(name);
  std::fs::write(&path, &out).unwrap();
  (digest, path)
}

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

#[test]
fn digest_round_trips_display_from_str_as_ref_and_oci_spec() {
  let d = Digest::of(b"oci-layer");
  let text = d.to_string();
  assert!(text.starts_with("sha256:"));
  assert_eq!(text.len(), 7 + 64);
  assert_eq!(text.parse::<Digest>().unwrap(), d);
  assert_eq!(d.as_ref().len(), 32);
  assert_eq!(d.as_ref(), d.as_bytes());
  let oci_d: oci_spec::image::Digest = d.into();
  assert_eq!(oci_d.as_ref(), text);

  assert!("md5:0000".parse::<Digest>().is_err());
  assert!("sha256:ZZZZ".parse::<Digest>().is_err());
}

#[test]
fn archive_canonical_matches_overlayfs_whiteouts_and_opaque_xattrs_to_oci() {
  let oci_raw =
    TarBuilder::new().dir("etc").file("etc/.wh.motd", b"").dir("var").file("var/.wh..wh..opq", b"").file("var/new", b"ok").bytes();
  let trusted_overlay = TarBuilder::new()
    .dir("etc")
    .overlay_whiteout("etc/motd")
    .overlay_opaque("var", "SCHILY.xattr.trusted.overlay.opaque")
    .file("var/new", b"ok")
    .bytes();
  let user_overlay = TarBuilder::new()
    .dir("etc")
    .overlay_whiteout("etc/motd")
    .overlay_opaque("var", "SCHILY.xattr.user.overlay.opaque")
    .file("var/new", b"ok")
    .bytes();

  let mut oci_canon = Vec::new();
  let mut trusted_canon = Vec::new();
  let mut user_canon = Vec::new();
  canonical(oci_raw.as_slice(), &mut oci_canon).unwrap();
  canonical(trusted_overlay.as_slice(), &mut trusted_canon).unwrap();
  canonical(user_overlay.as_slice(), &mut user_canon).unwrap();

  assert_eq!(Digest::of(&trusted_canon), Digest::of(&oci_canon));
  assert_eq!(Digest::of(&user_canon), Digest::of(&oci_canon));

  let dir = tempfile::tempdir().unwrap();
  let path = dir.path().join("layer.tar");
  std::fs::write(&path, &oci_canon).unwrap();
  let index = Index::read(&path).unwrap();
  assert!(index.whiteouts.contains(Path::new("etc/motd")));
  assert!(index.opaque.contains(Path::new("var")));
  assert!(index.entries.contains_key(Path::new("var/new")));
}

#[test]
fn archive_canonical_rejects_parent_traversal() {
  let mut header = tar_header(tar::EntryType::Regular, 0o644);
  header.set_size(1);
  header.as_old_mut().name[..9].copy_from_slice(b"../escape");
  header.set_cksum();
  let mut builder = tar::Builder::new(Vec::new());
  builder.append(&header, b"x".as_slice()).unwrap();
  let raw = builder.into_inner().unwrap();

  let mut out = Vec::new();
  let err = canonical(raw.as_slice(), &mut out).unwrap_err();
  assert!(matches!(err, Error::Invalid(_)), "{err}");
}

#[test]
fn collapse_preserves_whiteouts_opaque_dirs_and_broken_hardlinks() {
  let dir = tempfile::tempdir().unwrap();
  let (_, l1_path) = write_canonical_layer(
    dir.path(),
    "l1.tar",
    &TarBuilder::new()
      .dir("etc")
      .file("etc/a", b"orig-a")
      .file("etc/b", b"orig-b")
      .hardlink("etc/c", "etc/a")
      .file("etc/.wh.removed_below", b"")
      .file(".wh.recreated_dir", b"")
      .bytes(),
  );
  let (_, l2_path) = write_canonical_layer(
    dir.path(),
    "l2.tar",
    &TarBuilder::new()
      .dir("etc")
      .overlay_whiteout("etc/a")
      .file("etc/b", b"new-b")
      .dir("recreated_dir")
      .file("recreated_dir/fresh", b"yes")
      .bytes(),
  );

  let lower = Index::read(&l1_path).unwrap();
  let upper = Index::read(&l2_path).unwrap();
  let merged = collapse(&lower, &upper).unwrap();

  let mut merged_bytes = Vec::new();
  merged.write(&mut merged_bytes).unwrap();
  let merged_path = dir.path().join("merged.tar");
  std::fs::write(&merged_path, &merged_bytes).unwrap();
  let roundtripped = Index::read(&merged_path).unwrap();

  assert!(roundtripped.whiteouts.contains(Path::new("etc/a")));
  assert!(roundtripped.whiteouts.contains(Path::new("etc/removed_below")));
  assert!(!roundtripped.whiteouts.contains(Path::new("recreated_dir")));
  assert!(roundtripped.opaque.contains(Path::new("recreated_dir")));

  // Hardlink `etc/c` survived removal of `etc/a` with original content.
  let Kind::File(c_data) = &roundtripped.entries[Path::new("etc/c")].kind else {
    panic!("expected etc/c to be materialized as regular file");
  };
  let mut c_content = Vec::new();
  c_data.reader().read_to_end(&mut c_content).unwrap();
  assert_eq!(c_content, b"orig-a");

  let Kind::File(b_data) = &roundtripped.entries[Path::new("etc/b")].kind else {
    panic!("expected etc/b to be regular file");
  };
  let mut b_content = Vec::new();
  b_data.reader().read_to_end(&mut b_content).unwrap();
  assert_eq!(b_content, b"new-b");
}

#[cfg(feature = "squashfs")]
#[test]
fn squashfs_disk_round_trip_with_custom_layout_and_opaque_expansion() {
  use crate::disk::{Below, DiskLayout, hides_below, write};

  let dir = tempfile::tempdir().unwrap();
  let (d1, l1_path) = write_canonical_layer(
    dir.path(),
    "l1.tar",
    &TarBuilder::new()
      .dir("opt")
      .file("opt/old", b"old")
      .file("opt/kept", b"v1")
      .symlink("opt/link", "kept")
      .hardlink("opt/hard", "opt/kept")
      .bytes(),
  );
  let (d2, l2_path) = write_canonical_layer(
    dir.path(),
    "l2.tar",
    &TarBuilder::new().dir("opt").overlay_opaque("opt", "SCHILY.xattr.trusted.overlay.opaque").file("opt/kept", b"v2").bytes(),
  );

  let idx1 = Index::read(&l1_path).unwrap();
  let idx2 = Index::read(&l2_path).unwrap();
  assert!(!hides_below(&idx1));
  assert!(hides_below(&idx2));

  let layout = DiskLayout {
    marker: "layer-id",
    root: "tree",
  };
  let mut below = Below::default();
  let disk1 = dir.path().join("d1.squashfs");
  write(&idx1, &d1, &below, &layout, &disk1).unwrap();
  below.stack(&idx1);

  let disk2 = dir.path().join("d2.squashfs");
  write(&idx2, &d2, &below, &layout, &disk2).unwrap();

  let read_disk = |path: &Path| -> BTreeMap<String, String> {
    let bytes = std::fs::read(path).unwrap();
    let fs = backhand::FilesystemReader::from_reader(std::io::Cursor::new(bytes)).unwrap();
    let mut map = BTreeMap::new();
    for node in fs.files() {
      let what = match &node.inner {
        backhand::InnerNode::Dir(_) => "dir".to_string(),
        backhand::InnerNode::File(file) => {
          let mut content = String::new();
          fs.file(file).reader().read_to_string(&mut content).unwrap();
          format!("file:{content}")
        }
        backhand::InnerNode::Symlink(link) => format!("symlink:{}", link.link.display()),
        backhand::InnerNode::CharacterDevice(dev) if dev.device_number == 0 => "whiteout".to_string(),
        _ => "other".to_string(),
      };
      let parts: Vec<_> = node.fullpath.components().filter_map(|p| p.as_os_str().to_str().filter(|p| *p != "/" && *p != "\\")).collect();
      map.insert(format!("/{}", parts.join("/")), what);
    }
    map
  };

  let t1 = read_disk(&disk1);
  assert_eq!(t1["/layer-id"], format!("file:{d1}\n"));
  assert_eq!(t1["/tree/opt/kept"], "file:v1");
  assert_eq!(t1["/tree/opt/hard"], "file:v1");
  assert_eq!(t1["/tree/opt/link"], "symlink:kept");

  let t2 = read_disk(&disk2);
  assert_eq!(t2["/layer-id"], format!("file:{d2}\n"));
  assert_eq!(t2["/tree/opt/kept"], "file:v2");
  assert_eq!(t2["/tree/opt/old"], "whiteout");
  assert_eq!(t2["/tree/opt/link"], "whiteout");
  assert_eq!(t2["/tree/opt/hard"], "whiteout");
}

#[test]
fn oci_export_and_assemble_on_base_produce_valid_hybrid_archives() {
  let dir = tempfile::tempdir().unwrap();
  let (d1, l1_path) = write_canonical_layer(dir.path(), "l1.tar", &TarBuilder::new().dir("etc").file("etc/a", b"hello").bytes());
  let (d2, l2_path) = write_canonical_layer(dir.path(), "l2.tar", &TarBuilder::new().dir("opt").file("opt/b", b"world").bytes());

  // 1. Standalone export_layers produces a valid hybrid OCI + Docker archive.
  let exported = dir.path().join("exported.tar");
  export_layers(&[(d1, l1_path.clone())], "amd64", "linux", "example:base", &exported, "oci-layer-test", |cfg| {
    cfg["config"]["Labels"] = serde_json::json!({ "example.test": "1" });
  })
  .unwrap();

  let entries = read_tar_entries(&exported);
  let layout: OciLayout = serde_json::from_slice(&entries["oci-layout"]).unwrap();
  assert_eq!(layout.image_layout_version(), "1.0.0");

  let index: ImageIndex = serde_json::from_slice(&entries["index.json"]).unwrap();
  assert_eq!(index.manifests().len(), 1);
  let manifest_blob_path = blob_path(&index.manifests()[0].digest().as_ref().parse::<Digest>().unwrap());
  let manifest: ImageManifest = serde_json::from_slice(&entries[&manifest_blob_path]).unwrap();
  assert_eq!(manifest.layers().len(), 1);
  assert_eq!(*manifest.layers()[0].media_type(), MediaType::ImageLayer);

  // 2. Stack a second layer + synthesized extra layer on top of `exported` as base.
  let base = BaseArchive::open(&exported).unwrap();
  assert_eq!(base.find_regular_file_top_down("etc/a").unwrap().as_deref(), Some(b"hello".as_slice()));

  let extra_tar = TarBuilder::new().dir("extra").file("extra/hook", b"from-hook").bytes();
  let extra_digest = Digest::of(&extra_tar);
  let assembled = dir.path().join("assembled.tar");
  assemble_on_base(
    &base,
    &[(d2, l2_path)],
    "example:stacked",
    &assembled,
    "oci-layer-test",
    |_| Ok(Some((extra_digest, extra_tar.clone()))),
    |cfg| {
      cfg["config"]["Labels"]["example.stacked"] = serde_json::json!("yes");
    },
  )
  .unwrap();

  let stacked_entries = read_tar_entries(&assembled);
  let docker_manifest: Vec<serde_json::Value> = serde_json::from_slice(&stacked_entries["manifest.json"]).unwrap();
  assert_eq!(docker_manifest[0]["RepoTags"], serde_json::json!(["example:stacked"]));
  let layers: Vec<String> = serde_json::from_value(docker_manifest[0]["Layers"].clone()).unwrap();
  assert_eq!(layers.len(), 3, "base + l2 + extra hook layer");

  // 3. extract_top_layer returns the extra hook layer verbatim.
  let mut top_reader = extract_top_layer(&assembled).unwrap();
  let mut top_bytes = Vec::new();
  top_reader.read_to_end(&mut top_bytes).unwrap();
  assert_eq!(Digest::of(&top_bytes), extra_digest);
}

// ROOT CAUSE:
//
// If a hybrid OCI archive wrote a short Docker reference (`needy-snapshot:<hex>`)
// verbatim into `index.json`'s `io.containerd.image.name` annotation, Docker
// engines using the containerd image store (`io.containerd.snapshotter.v1`)
// imported the image under the unnormalized key, while `docker inspect` and
// `docker create` normalized the query to `docker.io/library/needy-snapshot:<hex>`
// and failed with 404.
//
// Before the fix, `io.containerd.image.name` copied `tag` verbatim.
// The fix normalizes `io.containerd.image.name` to Docker's canonical form while
// preserving `org.opencontainers.image.ref.name` and `manifest.json` `RepoTags`.
#[test]
fn oci_export_normalizes_containerd_image_name_annotation() {
  let dir = tempfile::tempdir().unwrap();
  let (d1, l1_path) = write_canonical_layer(dir.path(), "l1.tar", &TarBuilder::new().file("a", b"1").bytes());

  let cases = [
    ("needy-snapshot:deadbeef", "docker.io/library/needy-snapshot:deadbeef"),
    ("needy-snapshot", "docker.io/library/needy-snapshot:latest"),
    ("myorg/myrepo:v1", "docker.io/myorg/myrepo:v1"),
    ("ghcr.io/needy-dev/needy-actor:v0.0.7", "ghcr.io/needy-dev/needy-actor:v0.0.7"),
    ("localhost:5000/needy/actor:dev", "localhost:5000/needy/actor:dev"),
  ];

  for (idx, (tag, expected_containerd_name)) in cases.iter().enumerate() {
    let out = dir.path().join(format!("out-{idx}.tar"));
    export_layers(&[(d1, l1_path.clone())], "amd64", "linux", tag, &out, "oci-layer-test", |_| {}).unwrap();
    let entries = read_tar_entries(&out);
    let index: ImageIndex = serde_json::from_slice(&entries["index.json"]).unwrap();
    let annotations = index.manifests()[0].annotations().as_ref().expect("annotations present");
    assert_eq!(annotations.get("org.opencontainers.image.ref.name").map(String::as_str), Some(*tag));
    assert_eq!(annotations.get("io.containerd.image.name").map(String::as_str), Some(*expected_containerd_name));
    let docker_manifest: Vec<serde_json::Value> = serde_json::from_slice(&entries["manifest.json"]).unwrap();
    assert_eq!(docker_manifest[0]["RepoTags"], serde_json::json!([*tag]));
  }
}

#[test]
fn oci_base_archive_reads_gzip_oci_layout_without_manifest_json() {
  let dir = tempfile::tempdir().unwrap();
  let layer_bytes = TarBuilder::new().dir("etc").file("etc/probe", b"gz-ok").bytes();
  let diff_id = Digest::of(&layer_bytes);

  let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
  enc.write_all(&layer_bytes).unwrap();
  let gz_bytes = enc.finish().unwrap();
  let gz_digest = Digest::of(&gz_bytes);

  let config_bytes = serde_json::to_vec(&serde_json::json!({
    "architecture": "arm64",
    "os": "linux",
    "rootfs": { "type": "layers", "diff_ids": [diff_id.to_string()] }
  }))
  .unwrap();
  let config_digest = Digest::of(&config_bytes);

  let manifest_bytes = serde_json::to_vec(&serde_json::json!({
    "schemaVersion": 2,
    "mediaType": "application/vnd.oci.image.manifest.v1+json",
    "config": {
      "mediaType": "application/vnd.oci.image.config.v1+json",
      "digest": config_digest.to_string(),
      "size": config_bytes.len()
    },
    "layers": [{
      "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
      "digest": gz_digest.to_string(),
      "size": gz_bytes.len()
    }]
  }))
  .unwrap();
  let manifest_digest = Digest::of(&manifest_bytes);

  let index_bytes = serde_json::to_vec(&serde_json::json!({
    "schemaVersion": 2,
    "mediaType": "application/vnd.oci.image.index.v1+json",
    "manifests": [{
      "mediaType": "application/vnd.oci.image.manifest.v1+json",
      "digest": manifest_digest.to_string(),
      "size": manifest_bytes.len()
    }]
  }))
  .unwrap();

  let oci_tar = dir.path().join("oci-gz.tar");
  let mut builder = tar::Builder::new(File::create(&oci_tar).unwrap());
  let append = |b: &mut tar::Builder<File>, p: &str, data: &[u8]| {
    let mut h = tar_header(tar::EntryType::Regular, 0o644);
    h.set_size(data.len() as u64);
    b.append_data(&mut h, p, data).unwrap();
  };
  append(&mut builder, "oci-layout", br#"{"imageLayoutVersion":"1.0.0"}"#);
  append(&mut builder, "index.json", &index_bytes);
  append(&mut builder, &blob_path(&config_digest), &config_bytes);
  append(&mut builder, &blob_path(&manifest_digest), &manifest_bytes);
  append(&mut builder, &blob_path(&gz_digest), &gz_bytes);
  builder.finish().unwrap();

  let base = BaseArchive::open(&oci_tar).unwrap();
  assert_eq!(base.find_regular_file_top_down("etc/probe").unwrap().as_deref(), Some(b"gz-ok".as_slice()));

  let mut top = extract_top_layer(&oci_tar).unwrap();
  let mut unpacked = Vec::new();
  top.read_to_end(&mut unpacked).unwrap();
  assert_eq!(Digest::of(&unpacked), diff_id);
}
