use std::os::unix::fs::MetadataExt;

use super::*;
use crate::guest::init::testing::other_group;

// ROOT CAUSE:
//
// If a copy-in mapping's path had missing parents under the home, such as
// `~/.local` above `~/.local/share/opencode`, they stayed root's, and the
// agent could not create `~/.local/state` (`EACCES`), because init unpacked
// as root, the unpack created the parents, and only the mapping's own tree
// was given to the user.
//
// Before the fix, `~/.a` and `~/.a/b` kept init's owner.
// The fix keeps every directory copy-in creates between the home and the
// mapping's path owned by the user.
#[test]
fn copy_in_gives_the_user_every_directory_it_creates_under_the_home() {
  // The test runs as a user that is not root, so it gives the files to
  // one of its other groups, which is what tells the user's directories
  // apart from the ones it did not give away.
  let Some(gid) = other_group() else {
    eprintln!("skipped: this user has no group besides its own to give files to");
    return;
  };
  // SAFETY: `geteuid` cannot fail.
  let uid = unsafe { libc::geteuid() };
  let dir = tempfile::tempdir().unwrap();
  let source = dir.path().join("source");
  std::fs::create_dir_all(source.join("sub")).unwrap();
  std::fs::write(source.join("sub/file"), "x").unwrap();
  let archive = needy_common::archive::pack(&source, &[]).unwrap();
  let home = dir.path().join("home");
  std::fs::create_dir(&home).unwrap();

  place(&archive, &home.join(".a/b/c"), home.to_str().unwrap(), uid, gid).unwrap();

  for path in [".a", ".a/b", ".a/b/c", ".a/b/c/sub", ".a/b/c/sub/file"] {
    let meta = std::fs::symlink_metadata(home.join(path)).unwrap();
    assert_eq!((meta.uid(), meta.gid()), (uid, gid), "{path}");
  }
  let meta = std::fs::metadata(&home).unwrap();
  assert_ne!(meta.gid(), gid, "the home existed and keeps its owner");
}

#[test]
fn copy_in_outside_the_home_creates_its_parents_as_before() {
  // SAFETY: `geteuid` and `getegid` cannot fail.
  let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
  let dir = tempfile::tempdir().unwrap();
  let source = dir.path().join("source");
  std::fs::create_dir(&source).unwrap();
  std::fs::write(source.join("file"), "x").unwrap();
  let archive = needy_common::archive::pack(&source, &[]).unwrap();
  let home = dir.path().join("home");
  std::fs::create_dir(&home).unwrap();

  place(&archive, &dir.path().join("opt/x"), home.to_str().unwrap(), uid, gid).unwrap();

  assert_eq!(std::fs::read_to_string(dir.path().join("opt/x/file")).unwrap(), "x");
}

#[test]
fn push_chunks_streams_empty_single_and_multi_chunk_archives() {
  let empty: Vec<_> = push_chunks(Vec::new(), "empty".into()).collect();
  assert_eq!(empty.len(), 1);
  assert_eq!(empty[0].name, "empty");
  assert!(empty[0].data.is_empty());

  let small: Vec<_> = push_chunks(b"hello".to_vec(), "small".into()).collect();
  assert_eq!(small.len(), 1);
  assert_eq!(small[0].name, "small");
  assert_eq!(small[0].data, b"hello");

  let payload: Vec<u8> = (0..(2 * CHUNK + 19)).map(|i| (i % 251) as u8).collect();
  let multi: Vec<_> = push_chunks(payload.clone(), "multi".into()).collect();
  assert_eq!(multi.len(), 3);
  assert_eq!(multi[0].name, "multi");
  assert_eq!(multi[1].name, "");
  assert_eq!(multi[2].name, "");
  assert_eq!(multi[0].data.len(), CHUNK);
  assert_eq!(multi[1].data.len(), CHUNK);
  assert_eq!(multi[2].data.len(), 19);
  let reassembled: Vec<u8> = multi.into_iter().flat_map(|m| m.data).collect();
  assert_eq!(reassembled, payload);
}
