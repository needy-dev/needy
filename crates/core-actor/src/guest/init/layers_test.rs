use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use needy_api_proto::v1::GuestLayer;
use nix::mount::MntFlags;
use tokio::io::AsyncReadExt;

use super::*;
use crate::image::store::Store;

/// Unmounts `path` on drop so overlayfs tests never leave mounts behind.
struct Unmount(PathBuf);

impl Drop for Unmount {
  fn drop(&mut self) {
    let _ = nix::mount::umount2(&self.0, MntFlags::MNT_DETACH);
  }
}

fn is_root() -> bool {
  // SAFETY: `geteuid` cannot fail.
  unsafe { libc::geteuid() == 0 }
}

#[test]
fn layer_device_name_filter_skips_root_guest_and_partitions() {
  assert!(!is_layer_device_name("vda"));
  assert!(!is_layer_device_name("vdb"));
  assert!(is_layer_device_name("vdc"));
  assert!(is_layer_device_name("vdz"));
  assert!(is_layer_device_name("vdaa"));
  assert!(!is_layer_device_name("vdc1"));
  assert!(!is_layer_device_name("vd"));
  assert!(!is_layer_device_name("sda"));
  assert!(!is_layer_device_name("loop0"));
}

#[test]
fn matching_layer_disks_by_marker_orders_top_to_bottom() {
  let dir = tempfile::tempdir().unwrap();
  let disk_a = dir.path().join("vdd");
  let disk_b = dir.path().join("vdc");
  std::fs::create_dir_all(disk_a.join(LAYER_ROOT)).unwrap();
  std::fs::create_dir_all(disk_b.join(LAYER_ROOT)).unwrap();

  let mut mounted = HashMap::new();
  mounted.insert("sha256:aaaa".into(), disk_a.clone());
  mounted.insert("sha256:bbbb".into(), disk_b.clone());

  // `expected` is lowest first: aaaa below bbbb.
  let expected = vec![
    GuestLayer {
      digest: "sha256:aaaa".into(),
    },
    GuestLayer {
      digest: "sha256:bbbb".into(),
    },
  ];

  let roots = resolve_layer_roots(&expected, &mounted).unwrap();

  // Overlayfs `lowerdir` is top-to-bottom: bbbb over aaaa.
  assert_eq!(roots, vec![disk_b.join(LAYER_ROOT), disk_a.join(LAYER_ROOT)]);
}

#[test]
fn missing_layer_digest_fails_with_clear_error() {
  let dir = tempfile::tempdir().unwrap();
  let disk_a = dir.path().join("vdc");
  std::fs::create_dir_all(disk_a.join(LAYER_ROOT)).unwrap();

  let mut mounted = HashMap::new();
  mounted.insert("sha256:aaaa".into(), disk_a);

  let expected = vec![
    GuestLayer {
      digest: "sha256:aaaa".into(),
    },
    GuestLayer {
      digest: "sha256:missing".into(),
    },
  ];

  let err = resolve_layer_roots(&expected, &mounted).unwrap_err().to_string();
  assert!(err.contains("sha256:missing"), "error should name missing digest: {err}");
  assert!(err.contains("sha256:aaaa"), "error should list found digests: {err}");
}

#[test]
fn stacking_two_layers_over_a_root_shows_upper_file_and_hides_whiteout() {
  if !is_root() {
    eprintln!("skipped: mounting overlayfs needs root (run guest tests in a privileged Linux container)");
    return;
  }
  let dir = tempfile::tempdir().unwrap();
  // Mount a fresh tmpfs over `dir` so tests running inside a Docker
  // container (whose `/tmp` is itself overlayfs) can create char 0/0
  // whiteouts and use `upper`/`work` on a non-overlay filesystem.
  nix::mount::mount(Some("tmpfs"), dir.path(), Some("tmpfs"), MsFlags::empty(), Some("mode=755")).unwrap();
  let _tmpfs_guard = Unmount(dir.path().to_path_buf());

  let base = dir.path().join("base");
  let layer1 = dir.path().join("layer1").join(LAYER_ROOT);
  let layer2 = dir.path().join("layer2").join(LAYER_ROOT);
  let upper = dir.path().join("upper");
  let work = dir.path().join("work");
  let merged = dir.path().join("merged");
  for p in [&base, &layer1, &layer2, &upper, &work, &merged] {
    std::fs::create_dir_all(p).unwrap();
  }

  std::fs::write(base.join("keep.txt"), "from-base").unwrap();
  std::fs::write(base.join("override.txt"), "from-base").unwrap();
  std::fs::write(base.join("deleted.txt"), "from-base").unwrap();

  std::fs::write(layer1.join("override.txt"), "from-layer1").unwrap();
  std::fs::write(layer1.join("only-l1.txt"), "l1").unwrap();
  // Overlayfs whiteout in layer1: char device 0/0.
  nix::sys::stat::mknod(&layer1.join("deleted.txt"), nix::sys::stat::SFlag::S_IFCHR, nix::sys::stat::Mode::empty(), 0).unwrap();

  std::fs::write(layer2.join("override.txt"), "from-layer2").unwrap();
  std::fs::write(layer2.join("only-l2.txt"), "l2").unwrap();

  // `lowerdir` is top-to-bottom: layer2 over layer1 over base.
  mount_overlay(&[layer2, layer1, base], &upper, &work, &merged).unwrap();
  let _guard = Unmount(merged.clone());

  assert_eq!(std::fs::read_to_string(merged.join("keep.txt")).unwrap(), "from-base");
  assert_eq!(std::fs::read_to_string(merged.join("override.txt")).unwrap(), "from-layer2");
  assert_eq!(std::fs::read_to_string(merged.join("only-l1.txt")).unwrap(), "l1");
  assert_eq!(std::fs::read_to_string(merged.join("only-l2.txt")).unwrap(), "l2");
  assert!(!merged.join("deleted.txt").exists(), "whiteout in layer1 must hide base's deleted.txt");
}

#[test]
fn archiving_an_overlayfs_upper_dir_feeds_into_layer_store_put() {
  if !is_root() {
    eprintln!("skipped: mounting overlayfs needs root (run guest tests in a privileged Linux container)");
    return;
  }
  let dir = tempfile::tempdir().unwrap();
  // Fresh tmpfs so `upper`/`work` are not on a container's overlayfs `/tmp`.
  nix::mount::mount(Some("tmpfs"), dir.path(), Some("tmpfs"), MsFlags::empty(), Some("mode=755")).unwrap();
  let _tmpfs_guard = Unmount(dir.path().to_path_buf());

  let base = dir.path().join("base");
  let upper = dir.path().join("upper");
  let work = dir.path().join("work");
  let merged = dir.path().join("merged");
  for p in [&base, &upper, &work, &merged] {
    std::fs::create_dir_all(p).unwrap();
  }

  std::fs::write(base.join("keep.txt"), "base").unwrap();
  std::fs::write(base.join("remove-me.txt"), "gone").unwrap();
  std::fs::create_dir_all(base.join("replaced")).unwrap();
  std::fs::write(base.join("replaced/old.txt"), "old").unwrap();

  mount_overlay(std::slice::from_ref(&base), &upper, &work, &merged).unwrap();
  let _guard = Unmount(merged.clone());

  // Make changes through the merged view so the kernel populates `upper`
  // with regular files, a char 0/0 whiteout, and an opaque directory xattr.
  std::fs::write(merged.join("added.txt"), "hello").unwrap();
  std::fs::write(merged.join("keep.txt"), "updated").unwrap();
  std::fs::remove_file(merged.join("remove-me.txt")).unwrap();
  std::fs::remove_dir_all(merged.join("replaced")).unwrap();
  std::fs::create_dir(merged.join("replaced")).unwrap();
  std::fs::write(merged.join("replaced/new.txt"), "fresh").unwrap();
  std::os::unix::fs::symlink("added.txt", merged.join("sym")).unwrap();
  std::fs::hard_link(merged.join("added.txt"), merged.join("hard")).unwrap();

  let mut raw = Vec::new();
  archive_upper(&upper, &mut raw).unwrap();

  // Archive `base` too so `Store::materialize` can expand the opaque
  // directory `replaced/` against it, testing the full public `Store` API.
  let mut base_tar = Vec::new();
  archive_upper(&base, &mut base_tar).unwrap();

  let store_dir = tempfile::tempdir().unwrap();
  let store = Store::new(store_dir.path().join("cache"), store_dir.path().join("data"));
  let base_digest = store.put(base_tar.as_slice()).unwrap();
  let upper_digest = store.put(raw.as_slice()).unwrap();
  let snapshot = store.snapshot(&[base_digest, upper_digest]).unwrap();
  let disks = store.materialize(&snapshot).unwrap();
  assert_eq!(disks.len(), 2);

  // Inspect the materialized SquashFS disk of the upper layer with `backhand`.
  let file = std::io::BufReader::new(File::open(&disks[1].path).unwrap());
  let fs = backhand::FilesystemReader::from_reader(file).unwrap();
  let mut entries = BTreeSet::new();
  for node in fs.files() {
    let kind = match &node.inner {
      backhand::InnerNode::File(_) => "file",
      backhand::InnerNode::Dir(_) => "dir",
      backhand::InnerNode::Symlink(_) => "symlink",
      backhand::InnerNode::CharacterDevice(c) if c.device_number == 0 => "whiteout",
      _ => "other",
    };
    entries.insert(format!("{}:{kind}", node.fullpath.display()));
  }

  assert!(entries.contains("/root/added.txt:file"), "entries: {entries:?}");
  assert!(entries.contains("/root/keep.txt:file"), "entries: {entries:?}");
  assert!(entries.contains("/root/sym:symlink"), "entries: {entries:?}");
  assert!(entries.contains("/root/hard:file"), "entries: {entries:?}");
  assert!(entries.contains("/root/remove-me.txt:whiteout"), "char 0/0 whiteout must survive Store::put + materialize: {entries:?}");
  assert!(
    entries.contains("/root/replaced/old.txt:whiteout"),
    "opaque dir xattr must expand to whiteout of base's replaced/old.txt: {entries:?}"
  );
  assert!(entries.contains("/root/replaced/new.txt:file"), "entries: {entries:?}");
}

#[tokio::test]
async fn archive_stream_streams_valid_tar_and_propagates_errors() {
  let dir = tempfile::tempdir().unwrap();
  std::fs::write(dir.path().join("hello.txt"), "world").unwrap();

  let (mut reader, task) = archive_stream(dir.path().to_path_buf());
  let mut bytes = Vec::new();
  reader.read_to_end(&mut bytes).await.unwrap();
  task.await.unwrap().unwrap();

  let mut archive = tar::Archive::new(bytes.as_slice());
  let names: Vec<String> = archive.entries().unwrap().map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned()).collect();
  assert_eq!(names, vec!["hello.txt"]);

  // A missing upper directory must fail the reader with an error rather
  // than returning clean EOF (which would make `push_layer` send `last: true`).
  let (mut bad_reader, bad_task) = archive_stream(dir.path().join("does-not-exist"));
  let mut sink = Vec::new();
  let read_err = bad_reader.read_to_end(&mut sink).await;
  assert!(read_err.is_err(), "expected read error on missing upper dir");
  assert!(bad_task.await.unwrap().is_err());
}
