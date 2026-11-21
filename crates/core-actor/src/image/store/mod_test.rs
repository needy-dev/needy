use std::collections::BTreeMap;
use std::path::Path;

use super::*;

/// A layer archive a test writes entry by entry.
struct Archive(tar::Builder<Vec<u8>>);

impl Archive {
  fn new() -> Self {
    Self(tar::Builder::new(Vec::new()))
  }

  fn dir(mut self, path: &str) -> Self {
    let mut header = header(tar::EntryType::Directory, 0o755);
    self.0.append_data(&mut header, path, std::io::empty()).unwrap();
    self
  }

  fn file(mut self, path: &str, content: &str) -> Self {
    let mut header = header(tar::EntryType::Regular, 0o644);
    header.set_size(content.len() as u64);
    self.0.append_data(&mut header, path, content.as_bytes()).unwrap();
    self
  }

  fn symlink(mut self, path: &str, target: &str) -> Self {
    let mut header = header(tar::EntryType::Symlink, 0o777);
    self.0.append_link(&mut header, path, target).unwrap();
    self
  }

  fn hardlink(mut self, path: &str, target: &str) -> Self {
    let mut header = header(tar::EntryType::Link, 0o644);
    self.0.append_link(&mut header, path, target).unwrap();
    self
  }

  /// An overlayfs whiteout, as an upper directory holds it: a character
  /// device 0/0.
  fn overlay_whiteout(mut self, path: &str) -> Self {
    let mut header = header(tar::EntryType::Char, 0);
    header.set_device_major(0).unwrap();
    header.set_device_minor(0).unwrap();
    self.0.append_data(&mut header, path, std::io::empty()).unwrap();
    self
  }

  /// An overlayfs opaque directory, as an upper directory holds it: its
  /// `trusted.overlay.opaque` attribute in a PAX header.
  fn overlay_opaque(mut self, path: &str) -> Self {
    self.0.append_pax_extensions([("SCHILY.xattr.trusted.overlay.opaque", b"y".as_slice())]).unwrap();
    self.dir(path)
  }

  fn bytes(self) -> Vec<u8> {
    self.0.into_inner().unwrap()
  }
}

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

fn store(dir: &Path) -> Store {
  Store::new(dir.join("cache"), dir.join("data"))
}

fn put(store: &Store, archive: Archive) -> Digest {
  store.put(archive.bytes().as_slice()).unwrap()
}

/// What a layer disk holds, by path from its root: `dir`, `file:CONTENT`,
/// `symlink:TARGET`, `whiteout` (a character device 0/0), or `char:N`.
fn disk_tree(path: &Path) -> BTreeMap<String, String> {
  let bytes = std::fs::read(path).unwrap();
  let fs = backhand::FilesystemReader::from_reader(std::io::Cursor::new(bytes)).unwrap();
  let mut tree = BTreeMap::new();
  for node in fs.files() {
    let what = match &node.inner {
      backhand::InnerNode::Dir(_) => "dir".to_string(),
      backhand::InnerNode::File(file) => {
        let mut content = String::new();
        fs.file(file).reader().read_to_string(&mut content).unwrap();
        format!("file:{content}")
      }
      backhand::InnerNode::Symlink(link) => format!("symlink:{}", link.link.display()),
      backhand::InnerNode::CharacterDevice(device) if device.device_number == 0 => "whiteout".to_string(),
      backhand::InnerNode::CharacterDevice(device) => format!("char:{}", device.device_number),
      _ => "other".to_string(),
    };
    // The disk's own separator, whatever the host's (`\` on Windows).
    let parts: Vec<_> =
      node.fullpath.components().filter_map(|part| part.as_os_str().to_str().filter(|part| *part != "/" && *part != "\\")).collect();
    tree.insert(format!("/{}", parts.join("/")), what);
  }
  tree
}

/// The layer's tree on a disk: what the guest stacks, without the marker.
fn layer_tree(disk: &LayerDisk) -> BTreeMap<String, String> {
  let prefix = format!("/{}", crate::contract::LAYER_ROOT);
  disk_tree(&disk.path)
    .into_iter()
    .filter_map(|(path, what)| path.strip_prefix(&prefix).map(|rest| (rest.to_string(), what)))
    .filter(|(path, _)| !path.is_empty())
    .collect()
}

fn tree(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
  entries.iter().map(|(path, what)| (path.to_string(), what.to_string())).collect()
}

#[test]
fn put_names_a_layer_by_the_digest_of_its_canonical_archive() {
  let dir = tempfile::tempdir().unwrap();
  let store = store(dir.path());

  let one = put(&store, Archive::new().dir("etc").file("etc/hostname", "one\n"));
  let again = put(&store, Archive::new().dir("etc").file("etc/hostname", "one\n"));
  let two = put(&store, Archive::new().dir("etc").file("etc/hostname", "two\n"));

  assert_eq!(one, again, "the same content, the same digest");
  assert_ne!(one, two);
  let text = one.to_string();
  assert!(text.starts_with("sha256:") && text.len() == "sha256:".len() + 64, "{text}");
  assert_eq!(text.parse::<Digest>().unwrap(), one);
}

#[test]
fn put_accepts_an_overlay_upper_layer_as_the_same_layer_as_its_oci_form() {
  let dir = tempfile::tempdir().unwrap();
  let store = store(dir.path());

  let oci = put(&store, Archive::new().dir("etc").file("etc/.wh.motd", "").dir("var").file("var/.wh..wh..opq", "").file("var/new", "x"));
  let overlay = put(&store, Archive::new().dir("etc").overlay_whiteout("etc/motd").overlay_opaque("var").file("var/new", "x"));

  assert_eq!(overlay, oci);
}

#[test]
fn put_refuses_a_path_that_leaves_the_layer() {
  let dir = tempfile::tempdir().unwrap();
  let store = store(dir.path());
  let mut header = header(tar::EntryType::Regular, 0o644);
  header.set_size(1);
  // `append_data` refuses `..`; write the name into the header directly.
  header.as_old_mut().name[..9].copy_from_slice(b"../escape");
  header.set_cksum();
  let mut builder = tar::Builder::new(Vec::new());
  builder.append(&header, b"x".as_slice()).unwrap();

  let err = store.put(builder.into_inner().unwrap().as_slice()).unwrap_err();

  assert!(matches!(err, Error::Invalid(_)), "{err}");
}

#[test]
fn a_snapshot_is_named_by_its_stack_and_resolves_to_it() {
  let dir = tempfile::tempdir().unwrap();
  let store = store(dir.path());
  let base = put(&store, Archive::new().file("a", "a"));
  let top = put(&store, Archive::new().file("b", "b"));

  let snapshot = store.snapshot(&[base, top]).unwrap();

  assert_eq!(store.snapshot(&[base, top]).unwrap().digest, snapshot.digest);
  assert_ne!(store.snapshot(&[top, base]).unwrap().digest, snapshot.digest, "order is part of the stack");
  assert_eq!(store.resolve(&snapshot.digest).unwrap(), snapshot);
  assert_eq!(snapshot.layers, vec![base, top]);
  let unknown = Digest::of(b"no such layer");
  assert!(matches!(store.snapshot(&[base, unknown]), Err(Error::Missing(d)) if d == unknown));
  assert!(matches!(store.resolve(&unknown), Err(Error::Missing(d)) if d == unknown));
}

#[test]
fn materialize_gives_one_squashfs_disk_per_layer_with_its_digest_marker() {
  let dir = tempfile::tempdir().unwrap();
  let store = store(dir.path());
  let base = put(&store, Archive::new().dir("etc").file("etc/motd", "hi").symlink("etc/link", "motd").hardlink("etc/same", "etc/motd"));
  let top = put(&store, Archive::new().dir("etc").file("etc/.wh.motd", ""));
  let snapshot = store.snapshot(&[base, top]).unwrap();

  let disks = store.materialize(&snapshot).unwrap();

  assert_eq!(disks.iter().map(|disk| disk.digest).collect::<Vec<_>>(), vec![base, top]);
  for disk in &disks {
    assert!(disk.path.starts_with(dir.path().join("cache")), "derived disks are cache");
    let marker = disk_tree(&disk.path).remove(&format!("/{}", crate::contract::LAYER_MARKER)).unwrap();
    assert_eq!(marker, format!("file:{}\n", disk.digest));
  }
  assert_eq!(
    layer_tree(&disks[0]),
    tree(&[
      ("/etc", "dir"),
      ("/etc/link", "symlink:motd"),
      ("/etc/motd", "file:hi"),
      ("/etc/same", "file:hi")
    ])
  );
  assert_eq!(layer_tree(&disks[1]), tree(&[("/etc", "dir"), ("/etc/motd", "whiteout")]), "an OCI whiteout becomes overlayfs's");
  assert_eq!(store.materialize(&snapshot).unwrap(), disks, "made once, then reused");
}

#[test]
fn materialize_expands_an_opaque_directory_into_whiteouts_of_what_it_hides() {
  let dir = tempfile::tempdir().unwrap();
  let store = store(dir.path());
  let base = put(
    &store,
    Archive::new().dir("opt").file("opt/gone", "1").file("opt/kept", "1").dir("opt/sub").file("opt/sub/old", "1").file("other", "1"),
  );
  let middle = put(&store, Archive::new().dir("opt").file("opt/added", "2").file("opt/.wh.gone", ""));
  let top =
    put(&store, Archive::new().dir("opt").file("opt/.wh..wh..opq", "").file("opt/kept", "3").dir("opt/sub").file("opt/sub/new", "3"));
  let snapshot = store.snapshot(&[base, middle, top]).unwrap();

  let disks = store.materialize(&snapshot).unwrap();

  // The SquashFS writer keeps no `trusted.overlay.opaque`, so every entry
  // the lower layers still show under `/opt` is whited out, down through a
  // directory the opaque one keeps (`/opt/sub`), and nothing else.
  assert_eq!(
    layer_tree(&disks[2]),
    tree(&[
      ("/opt", "dir"),
      ("/opt/added", "whiteout"),
      ("/opt/kept", "file:3"),
      ("/opt/sub", "dir"),
      ("/opt/sub/new", "file:3"),
      ("/opt/sub/old", "whiteout"),
    ])
  );
  // The same layer over another stack hides other entries: its disk is
  // another disk.
  let alone = store.materialize(&store.snapshot(&[top]).unwrap()).unwrap();
  assert_eq!(
    layer_tree(&alone[0]),
    tree(&[
      ("/opt", "dir"),
      ("/opt/kept", "file:3"),
      ("/opt/sub", "dir"),
      ("/opt/sub/new", "file:3")
    ])
  );
  assert_ne!(alone[0].path, disks[2].path);
  assert_eq!(alone[0].digest, top);
}

#[test]
fn commits_collapse_into_one_layer_and_keep_their_history() {
  let dir = tempfile::tempdir().unwrap();
  let store = store(dir.path());
  let base = put(&store, Archive::new().dir("etc").file("etc/base", "base").dir("srv").file("srv/old", "base"));
  let edit = |archive: Archive| archive.bytes();
  let commands = |line: &str| {
    vec![Command::new(
      vec!["sh".into(), "-c".into(), line.into()],
      "/root",
    )]
  };

  let first = store
    .commit(
      "image",
      None,
      Some(&base),
      edit(Archive::new().dir("etc").file("etc/a", "1").file("etc/b", "1").hardlink("etc/c", "etc/a")).as_slice(),
      commands("one"),
    )
    .unwrap();
  let second = store
    .commit(
      "image",
      Some(&first),
      Some(&base),
      edit(Archive::new().dir("etc").overlay_whiteout("etc/a").overlay_whiteout("etc/base").file("etc/b", "2").overlay_opaque("srv"))
        .as_slice(),
      commands("two"),
    )
    .unwrap();

  // One layer holds both edits: the later one wins, its whiteouts stay for
  // the layers below, and a hard link whose target went away keeps its
  // content.
  let disks = store.materialize(&store.snapshot(&[base, second]).unwrap()).unwrap();
  assert_eq!(disks.len(), 2);
  assert!(disks[1].path.starts_with(dir.path().join("cache")));
  assert_eq!(
    layer_tree(&disks[1]),
    tree(&[
      ("/etc", "dir"),
      ("/etc/a", "whiteout"),
      ("/etc/b", "file:2"),
      ("/etc/base", "whiteout"),
      ("/etc/c", "file:1"),
      ("/srv", "dir"),
      ("/srv/old", "whiteout"),
    ])
  );
  let history = store.history("image").unwrap();
  assert_eq!(
    history.iter().map(|commit| (commit.layer, commit.parent, commit.base)).collect::<Vec<_>>(),
    vec![(first, None, Some(base)), (second, Some(first), Some(base))]
  );
  assert_eq!(history[1].commands, commands("two"));
  assert!(store.history("another image").unwrap().is_empty());
  // A commit's parent must be one of the image's commits.
  let stranger = put(&store, Archive::new().file("x", "x"));
  assert!(matches!(store.commit("image", Some(&stranger), None, edit(Archive::new()).as_slice(), vec![]), Err(Error::UnknownCommit { .. })));
}

#[test]
fn rollback_walks_back_the_commits_an_image_made() {
  let dir = tempfile::tempdir().unwrap();
  let store = store(dir.path());
  let edit = |content: &str| Archive::new().file("f", content).bytes();
  let base1 = Digest::of(b"base-1");
  let base2 = Digest::of(b"base-2");
  let first = store.commit("image", None, Some(&base1), edit("1").as_slice(), vec![]).unwrap();
  let second = store.commit("image", Some(&first), Some(&base2), edit("2").as_slice(), vec![]).unwrap();
  let third = store.commit("image", Some(&second), Some(&base2), edit("3").as_slice(), vec![]).unwrap();

  let back1 = store.rollback("image", &third, 1).unwrap().unwrap();
  assert_eq!((back1.layer, back1.base), (second, Some(base2)));
  let back2 = store.rollback("image", &third, 2).unwrap().unwrap();
  assert_eq!((back2.layer, back2.base), (first, Some(base1)));
  assert_eq!(store.rollback("image", &third, 3).unwrap(), None, "back past the first commit: no commit layer");
  assert!(matches!(
    store.rollback("image", &third, 4),
    Err(Error::TooFar {
      steps: 4,
      available: 3,
      ..
    })
  ));
  assert!(matches!(store.rollback("other", &third, 1), Err(Error::UnknownCommit { .. })));
  // A rolled-back commit stays in the history and can still be booted.
  let after = store.commit("image", Some(&first), Some(&base1), edit("4").as_slice(), vec![]).unwrap();
  let back_after = store.rollback("image", &after, 1).unwrap().unwrap();
  assert_eq!((back_after.layer, back_after.base), (first, Some(base1)));
  let disks = store.materialize(&store.snapshot(&[third]).unwrap()).unwrap();
  assert_eq!(layer_tree(&disks[0]), tree(&[("/f", "file:3")]));
}

#[test]
fn rollback_reads_legacy_history_without_base_field() {
  let dir = tempfile::tempdir().unwrap();
  let store = store(dir.path());
  let first = store.commit("legacy", None, None, Archive::new().file("f", "1").bytes().as_slice(), vec![]).unwrap();
  let second = store.commit("legacy", Some(&first), None, Archive::new().file("f", "2").bytes().as_slice(), vec![]).unwrap();

  // Overwrite history.json without the optional `base` field to simulate a
  // store written before base persistence landed.
  let legacy_json = serde_json::json!({
    "image": "legacy",
    "commits": [
      { "layer": first.to_string(), "parent": null, "commands": [], "time_ms": 1 },
      { "layer": second.to_string(), "parent": first.to_string(), "commands": [], "time_ms": 2 }
    ]
  });
  std::fs::write(store.history_path("legacy"), serde_json::to_vec(&legacy_json).unwrap()).unwrap();

  let rolled = store.rollback("legacy", &second, 1).unwrap().unwrap();
  assert_eq!(rolled.layer, first);
  assert_eq!(rolled.base, None);
}

#[test]
fn garbage_collection_keeps_named_layers_and_every_commit() {
  let dir = tempfile::tempdir().unwrap();
  let store = store(dir.path());
  let base = put(&store, Archive::new().file("base", "base"));
  let built = put(&store, Archive::new().file("built", "built"));
  let unused = put(&store, Archive::new().file("unused", "unused"));
  let named = put(&store, Archive::new().file("named", "named"));
  let commit = store.commit("image", None, None, Archive::new().file("edit", "edit").bytes().as_slice(), vec![]).unwrap();
  let booted = store.snapshot(&[base, built, commit]).unwrap();
  let disks = store.materialize(&booted).unwrap();
  let dropped = store.snapshot(&[unused]).unwrap();
  let dropped_disks = store.materialize(&dropped).unwrap();

  let collected = store.collect(&[booted.digest, named]).unwrap();

  assert_eq!(collected.layers, 1);
  assert!(collected.bytes > 0);
  assert!(!dropped_disks[0].path.exists());
  assert!(matches!(store.resolve(&dropped.digest), Err(Error::Missing(_))));
  assert!(matches!(store.snapshot(&[unused]), Err(Error::Missing(d)) if d == unused));
  assert_eq!(store.resolve(&booted.digest).unwrap(), booted);
  assert!(disks.iter().all(|disk| disk.path.exists()));
  store.snapshot(&[named]).unwrap();

  // Named by nothing but its image's history, a commit is still kept: it is
  // data, not cache.
  store.collect(&[]).unwrap();
  assert!(matches!(store.snapshot(&[base]), Err(Error::Missing(_))));
  let disks = store.materialize(&store.snapshot(&[commit]).unwrap()).unwrap();
  assert_eq!(layer_tree(&disks[0]), tree(&[("/edit", "file:edit")]));
}

#[test]
fn a_directory_an_edit_puts_back_over_a_removed_one_still_hides_what_was_below() {
  let dir = tempfile::tempdir().unwrap();
  let store = store(dir.path());
  let base = put(&store, Archive::new().dir("srv").file("srv/old", "base"));
  let first = store.commit("image", None, None, Archive::new().overlay_whiteout("srv").bytes().as_slice(), vec![]).unwrap();
  let second = store.commit("image", Some(&first), None, Archive::new().dir("srv").file("srv/new", "2").bytes().as_slice(), vec![]).unwrap();

  let disks = store.materialize(&store.snapshot(&[base, second]).unwrap()).unwrap();

  assert_eq!(
    layer_tree(&disks[1]),
    tree(&[
      ("/srv", "dir"),
      ("/srv/new", "file:2"),
      ("/srv/old", "whiteout")
    ])
  );
}

#[test]
fn materialize_auto_flattens_lower_layers_above_threshold_and_caches_result() {
  let dir = tempfile::tempdir().unwrap();
  let store = store(dir.path());

  // 10 layers: with MAX_UNFLATTENED_LAYERS == 8, layers 0..3 collapse into
  // disk 0, and layers 3..10 stay individual (disks 1..8).
  let l0 = put(
    &store,
    Archive::new()
      .dir("etc")
      .file("etc/kept", "v0")
      .file("etc/overwritten", "v0")
      .file("etc/deleted", "v0")
      .dir("opt")
      .file("opt/old", "v0")
      .dir("var")
      .file("var/from_lower", "v0"),
  );
  let l1 = put(&store, Archive::new().dir("etc").file("etc/overwritten", "v1").file("etc/.wh.deleted", "").file("etc/.wh.from_base", ""));
  let l2 = put(&store, Archive::new().dir("opt").file("opt/.wh..wh..opq", "").file("opt/fresh", "v2"));
  let l3 = put(&store, Archive::new().file("l3", "3"));
  let l4 = put(&store, Archive::new().file("l4", "4"));
  let l5 = put(&store, Archive::new().file("l5", "5"));
  let l6 = put(&store, Archive::new().file("l6", "6"));
  let l7 = put(&store, Archive::new().file("l7", "7"));
  let l8 = put(&store, Archive::new().dir("var").file("var/.wh..wh..opq", "").file("var/from_upper", "8"));
  let l9 = put(&store, Archive::new().file("l9", "9"));

  let layers = vec![l0, l1, l2, l3, l4, l5, l6, l7, l8, l9];
  assert!(layers.len() > MAX_UNFLATTENED_LAYERS);
  let snapshot = store.snapshot(&layers).unwrap();

  let disks = store.materialize(&snapshot).unwrap();
  assert_eq!(disks.len(), MAX_UNFLATTENED_LAYERS);

  // Disk 0 holds the collapsed prefix (l0..l3) and carries its marker.
  let prefix_digest = stack_digest(&layers[..3]);
  assert_eq!(disks[0].digest, prefix_digest);
  let marker = disk_tree(&disks[0].path).remove(&format!("/{}", crate::contract::LAYER_MARKER)).unwrap();
  assert_eq!(marker, format!("file:{prefix_digest}\n"));
  assert_eq!(
    layer_tree(&disks[0]),
    tree(&[
      ("/etc", "dir"),
      ("/etc/deleted", "whiteout"),
      ("/etc/from_base", "whiteout"),
      ("/etc/kept", "file:v0"),
      ("/etc/overwritten", "file:v1"),
      ("/opt", "dir"),
      ("/opt/fresh", "file:v2"),
      ("/var", "dir"),
      ("/var/from_lower", "file:v0"),
    ])
  );

  // Upper layers 3..10 remain individual disks with their own digests.
  let upper_digests: Vec<_> = disks[1..].iter().map(|d| d.digest).collect();
  assert_eq!(upper_digests, layers[3..]);
  // Layer 8 (at disks[6]) made `/var` opaque, so it must white out `/var/from_lower`
  // from the collapsed lower prefix!
  assert_eq!(
    layer_tree(&disks[6]),
    tree(&[
      ("/var", "dir"),
      ("/var/from_lower", "whiteout"),
      ("/var/from_upper", "file:8"),
    ])
  );

  // Second materialization is a cache hit (same paths and untouched flattened disk).
  let flat_mtime = std::fs::metadata(&disks[0].path).unwrap().modified().unwrap();
  let disks_again = store.materialize(&snapshot).unwrap();
  assert_eq!(disks_again, disks);
  assert_eq!(std::fs::metadata(&disks[0].path).unwrap().modified().unwrap(), flat_mtime);

  // Garbage collection preserves the flattened disk while the snapshot is kept,
  // and removes it once nothing keeps the snapshot.
  store.collect(&[snapshot.digest]).unwrap();
  assert!(disks[0].path.exists(), "kept snapshot must preserve its cached flattened disk");
  store.collect(&[]).unwrap();
  assert!(!disks[0].path.exists(), "unreferenced flattened disk must be collected");
}
