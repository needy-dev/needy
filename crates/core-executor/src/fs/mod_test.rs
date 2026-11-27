use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use needy_api_proto::v1;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use super::{FileName, HostFs, ROOT_INO, errno, linux_flags};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn req(body: v1::session_request::Body) -> v1::SessionRequest {
  v1::SessionRequest {
    id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
    body: Some(body),
  }
}

async fn call(fs: &HostFs, body: v1::session_request::Body) -> v1::session_response::Body {
  let r = req(body);
  let id = r.id;
  let resp = fs.handle(r).await;
  assert_eq!(resp.id, id);
  resp.body.expect("response body present")
}

fn expect_err(body: v1::session_response::Body, expected_errno: i32) {
  match body {
    v1::session_response::Body::Error(e) => {
      assert_eq!(e.errno, expected_errno, "expected errno {expected_errno}, got {} ({})", e.errno, e.message);
    }
    other => panic!("expected error {expected_errno}, got {other:?}"),
  }
}

#[tokio::test]
async fn filename_validation_rejects_escapes_and_nul() {
  for bad in ["", ".", "..", "a/b", "/abs", "bad\0name"] {
    let err = FileName::parse(bad).expect_err(&format!("should reject {bad:?}"));
    assert_eq!(err.errno, errno::EINVAL);
  }
  assert_eq!(FileName::parse("hello.txt").unwrap().as_str(), "hello.txt");
  assert_eq!(FileName::parse(".hidden").unwrap().as_str(), ".hidden");
  assert_eq!(FileName::parse("..dots").unwrap().as_str(), "..dots");
}

#[tokio::test]
async fn lookup_getattr_setattr_and_readdirplus() {
  let tmp = tempfile::tempdir().unwrap();
  let fs = HostFs::open(tmp.path(), true, v1::FsLockMode::Local).unwrap();

  let root = fs.root_attr();
  assert_eq!(root.ino, ROOT_INO);
  assert_eq!(root.kind, v1::FsFileType::Directory as i32);

  // Create a subdirectory and two files.
  let sub_entry = match call(
    &fs,
    v1::session_request::Body::Mkdir(v1::FsMkdir {
      parent: ROOT_INO,
      name: "sub".into(),
      mode: 0o755,
    }),
  )
  .await
  {
    v1::session_response::Body::Entry(e) => e,
    other => panic!("expected Entry, got {other:?}"),
  };
  let sub_ino = sub_entry.attr.unwrap().ino;
  assert_ne!(sub_ino, ROOT_INO);

  let created = match call(
    &fs,
    v1::session_request::Body::CreateAndWrite(v1::FsCreateAndWrite {
      parent: sub_ino,
      name: "hello.txt".into(),
      mode: 0o644,
      data: b"hello world".to_vec(),
    }),
  )
  .await
  {
    v1::session_response::Body::Entry(e) => e.attr.unwrap(),
    other => panic!("expected Entry, got {other:?}"),
  };
  assert_eq!(created.size, 11);

  // Lookup returns the same inode and attributes.
  let looked_up = match call(
    &fs,
    v1::session_request::Body::Lookup(v1::FsLookup {
      parent: sub_ino,
      name: "hello.txt".into(),
    }),
  )
  .await
  {
    v1::session_response::Body::Entry(e) => e.attr.unwrap(),
    other => panic!("expected Entry, got {other:?}"),
  };
  assert_eq!(looked_up.ino, created.ino);
  assert_eq!(looked_up.size, 11);

  // SetAttr truncates and updates mode & timestamps.
  let updated = match call(
    &fs,
    v1::session_request::Body::Setattr(v1::FsSetAttr {
      ino: created.ino,
      fh: None,
      mode: Some(0o600),
      size: Some(5),
      atime_sec: Some(1_700_000_000),
      atime_nsec: Some(0),
      mtime_sec: Some(1_700_000_123),
      mtime_nsec: Some(0),
    }),
  )
  .await
  {
    v1::session_response::Body::Attr(a) => a.attr.unwrap(),
    other => panic!("expected Attr, got {other:?}"),
  };
  assert_eq!(updated.size, 5);
  assert_eq!(updated.mtime_sec, 1_700_000_123);
  #[cfg(unix)]
  assert_eq!(updated.perm & 0o777, 0o600);

  // GetAttr matches SetAttr.
  let got = match call(
    &fs,
    v1::session_request::Body::Getattr(v1::FsGetAttr {
      ino: created.ino,
      fh: None,
    }),
  )
  .await
  {
    v1::session_response::Body::Attr(a) => a.attr.unwrap(),
    other => panic!("expected Attr, got {other:?}"),
  };
  assert_eq!(got.size, 5);

  // ReadDirPlus returns entries with full attributes and pagination.
  let _ = call(
    &fs,
    v1::session_request::Body::CreateAndWrite(v1::FsCreateAndWrite {
      parent: sub_ino,
      name: "second.txt".into(),
      mode: 0o644,
      data: b"abc".to_vec(),
    }),
  )
  .await;

  let page1 = match call(
    &fs,
    v1::session_request::Body::Readdirplus(v1::FsReadDirPlus {
      ino: sub_ino,
      offset: 0,
      max_entries: 1,
    }),
  )
  .await
  {
    v1::session_response::Body::Readdirplus(r) => r,
    other => panic!("expected Readdirplus, got {other:?}"),
  };
  assert_eq!(page1.entries.len(), 1);
  assert!(!page1.eof);
  assert_eq!(page1.entries[0].name, "hello.txt");
  assert_eq!(page1.entries[0].attr.unwrap().size, 5);

  let page2 = match call(
    &fs,
    v1::session_request::Body::Readdirplus(v1::FsReadDirPlus {
      ino: sub_ino,
      offset: page1.entries[0].offset,
      max_entries: 10,
    }),
  )
  .await
  {
    v1::session_response::Body::Readdirplus(r) => r,
    other => panic!("expected Readdirplus, got {other:?}"),
  };
  assert_eq!(page2.entries.len(), 1);
  assert!(page2.eof);
  assert_eq!(page2.entries[0].name, "second.txt");
  assert_eq!(page2.entries[0].attr.unwrap().size, 3);
}

#[tokio::test]
async fn create_write_read_flush_fsync_release() {
  let tmp = tempfile::tempdir().unwrap();
  let fs = HostFs::open(tmp.path(), true, v1::FsLockMode::Local).unwrap();

  // Create with O_WRONLY + writeback: true (should promote to O_RDWR).
  let created = match call(
    &fs,
    v1::session_request::Body::Create(v1::FsCreate {
      parent: ROOT_INO,
      name: "data.bin".into(),
      mode: 0o644,
      flags: linux_flags::O_WRONLY | linux_flags::O_APPEND,
      writeback: true,
    }),
  )
  .await
  {
    v1::session_response::Body::Created(c) => c,
    other => panic!("expected Created, got {other:?}"),
  };
  let ino = created.attr.unwrap().ino;
  let fh = created.fh;
  assert_eq!(created.open_flags & linux_flags::O_ACCMODE, linux_flags::O_RDWR);
  assert_eq!(created.open_flags & linux_flags::O_APPEND, 0);

  let written = match call(
    &fs,
    v1::session_request::Body::Write(v1::FsWrite {
      ino,
      fh,
      offset: 0,
      data: b"0123456789".to_vec(),
    }),
  )
  .await
  {
    v1::session_response::Body::Written(w) => w.written,
    other => panic!("expected Written, got {other:?}"),
  };
  assert_eq!(written, 10);

  // Because writeback promoted O_WRONLY -> O_RDWR, we can read from the same fh.
  let data = match call(
    &fs,
    v1::session_request::Body::Read(v1::FsRead {
      ino,
      fh,
      offset: 2,
      size: 4,
    }),
  )
  .await
  {
    v1::session_response::Body::Data(d) => d,
    other => panic!("expected Data, got {other:?}"),
  };
  assert_eq!(data.data, b"2345");
  assert!(!data.eof);

  assert!(matches!(
    call(
      &fs,
      v1::session_request::Body::Flush(v1::FsFlush {
        ino,
        fh,
        lock_owner: 0,
      }),
    )
    .await,
    v1::session_response::Body::Ok(_)
  ));

  assert!(matches!(
    call(
      &fs,
      v1::session_request::Body::Fsync(v1::FsFsync {
        ino,
        fh,
        datasync: true,
      }),
    )
    .await,
    v1::session_response::Body::Ok(_)
  ));

  assert!(matches!(
    call(
      &fs,
      v1::session_request::Body::Release(v1::FsRelease {
        ino,
        fh,
        lock_owner: 0,
      }),
    )
    .await,
    v1::session_response::Body::Ok(_)
  ));

  // Reading after Release returns EBADF.
  expect_err(
    call(
      &fs,
      v1::session_request::Body::Read(v1::FsRead {
        ino,
        fh,
        offset: 0,
        size: 4,
      }),
    )
    .await,
    errno::EBADF,
  );
}

#[tokio::test]
async fn open_unlinked_file_read_write_and_atomic_rename_over_open_file() {
  let tmp = tempfile::tempdir().unwrap();
  let fs = HostFs::open(tmp.path(), true, v1::FsLockMode::Local).unwrap();

  // Create and hold an open handle.
  let created = match call(
    &fs,
    v1::session_request::Body::Create(v1::FsCreate {
      parent: ROOT_INO,
      name: "ephemeral.txt".into(),
      mode: 0o644,
      flags: linux_flags::O_RDWR,
      writeback: false,
    }),
  )
  .await
  {
    v1::session_response::Body::Created(c) => c,
    other => panic!("expected Created, got {other:?}"),
  };
  let ino = created.attr.unwrap().ino;
  let fh = created.fh;

  // Unlink while open.
  assert!(matches!(
    call(
      &fs,
      v1::session_request::Body::Unlink(v1::FsUnlink {
        parent: ROOT_INO,
        name: "ephemeral.txt".into(),
      }),
    )
    .await,
    v1::session_response::Body::Ok(_)
  ));

  // Lookup now returns ENOENT, but write + read on the open fh still succeed.
  expect_err(
    call(
      &fs,
      v1::session_request::Body::Lookup(v1::FsLookup {
        parent: ROOT_INO,
        name: "ephemeral.txt".into(),
      }),
    )
    .await,
    errno::ENOENT,
  );

  let _ = call(
    &fs,
    v1::session_request::Body::Write(v1::FsWrite {
      ino,
      fh,
      offset: 0,
      data: b"still alive".to_vec(),
    }),
  )
  .await;

  let read = match call(
    &fs,
    v1::session_request::Body::Read(v1::FsRead {
      ino,
      fh,
      offset: 0,
      size: 64,
    }),
  )
  .await
  {
    v1::session_response::Body::Data(d) => d,
    other => panic!("expected Data, got {other:?}"),
  };
  assert_eq!(read.data, b"still alive");
  assert!(read.eof);

  let _ = call(
    &fs,
    v1::session_request::Body::Release(v1::FsRelease {
      ino,
      fh,
      lock_owner: 0,
    }),
  )
  .await;

  // Atomic rename over an open target file.
  let target = match call(
    &fs,
    v1::session_request::Body::CreateAndWrite(v1::FsCreateAndWrite {
      parent: ROOT_INO,
      name: "target.txt".into(),
      mode: 0o644,
      data: b"old version".to_vec(),
    }),
  )
  .await
  {
    v1::session_response::Body::Entry(e) => e.attr.unwrap(),
    other => panic!("expected Entry, got {other:?}"),
  };

  let opened_old = match call(
    &fs,
    v1::session_request::Body::Open(v1::FsOpen {
      ino: target.ino,
      flags: linux_flags::O_RDONLY,
      writeback: false,
    }),
  )
  .await
  {
    v1::session_response::Body::Opened(o) => o,
    other => panic!("expected Opened, got {other:?}"),
  };

  let _ = call(
    &fs,
    v1::session_request::Body::CreateAndWrite(v1::FsCreateAndWrite {
      parent: ROOT_INO,
      name: "staging.txt".into(),
      mode: 0o644,
      data: b"new version!".to_vec(),
    }),
  )
  .await;

  assert!(matches!(
    call(
      &fs,
      v1::session_request::Body::Rename(v1::FsRename {
        parent: ROOT_INO,
        name: "staging.txt".into(),
        new_parent: ROOT_INO,
        new_name: "target.txt".into(),
        flags: 0,
      }),
    )
    .await,
    v1::session_response::Body::Ok(_)
  ));

  // Existing open handle still reads the old version.
  let old_data = match call(
    &fs,
    v1::session_request::Body::Read(v1::FsRead {
      ino: target.ino,
      fh: opened_old.fh,
      offset: 0,
      size: 64,
    }),
  )
  .await
  {
    v1::session_response::Body::Data(d) => d.data,
    other => panic!("expected Data, got {other:?}"),
  };
  assert_eq!(old_data, b"old version");

  // New lookup sees the replacement file.
  let new_entry = match call(
    &fs,
    v1::session_request::Body::Lookup(v1::FsLookup {
      parent: ROOT_INO,
      name: "target.txt".into(),
    }),
  )
  .await
  {
    v1::session_response::Body::Entry(e) => e.attr.unwrap(),
    other => panic!("expected Entry, got {other:?}"),
  };
  assert_eq!(new_entry.size, 12);

  let _ = call(
    &fs,
    v1::session_request::Body::Release(v1::FsRelease {
      ino: target.ino,
      fh: opened_old.fh,
      lock_owner: 0,
    }),
  )
  .await;
}

#[tokio::test]
async fn relative_and_absolute_symlinks() {
  let tmp = tempfile::tempdir().unwrap();
  let fs = HostFs::open(tmp.path(), true, v1::FsLockMode::Local).unwrap();

  for (name, target) in [
    ("rel_link", "../sibling/file.txt"),
    ("abs_link", "/usr/bin/env"),
  ] {
    let entry = match call(
      &fs,
      v1::session_request::Body::Symlink(v1::FsSymlink {
        parent: ROOT_INO,
        name: name.into(),
        link_target: target.into(),
      }),
    )
    .await
    {
      v1::session_response::Body::Entry(e) => e.attr.unwrap(),
      other => panic!("expected Entry, got {other:?}"),
    };
    assert_eq!(entry.kind, v1::FsFileType::Symlink as i32);

    let got = match call(&fs, v1::session_request::Body::Readlink(v1::FsReadLink { ino: entry.ino })).await {
      v1::session_response::Body::LinkTarget(t) => t.target,
      other => panic!("expected LinkTarget, got {other:?}"),
    };
    assert_eq!(got, target);
  }
}

#[tokio::test]
async fn filename_escape_rejected_through_rpc() {
  let tmp = tempfile::tempdir().unwrap();
  let fs = HostFs::open(tmp.path(), true, v1::FsLockMode::Local).unwrap();

  for bad in ["..", ".", "a/b", "evil\0name"] {
    expect_err(
      call(
        &fs,
        v1::session_request::Body::Lookup(v1::FsLookup {
          parent: ROOT_INO,
          name: bad.into(),
        }),
      )
      .await,
      errno::EINVAL,
    );
  }
}

#[tokio::test]
async fn read_only_mount_enforces_erofs() {
  let tmp = tempfile::tempdir().unwrap();
  std::fs::write(tmp.path().join("seed.txt"), b"readonly").unwrap();
  let fs = HostFs::open(tmp.path(), false, v1::FsLockMode::Local).unwrap();

  let seed = match call(
    &fs,
    v1::session_request::Body::Lookup(v1::FsLookup {
      parent: ROOT_INO,
      name: "seed.txt".into(),
    }),
  )
  .await
  {
    v1::session_response::Body::Entry(e) => e.attr.unwrap(),
    other => panic!("expected Entry, got {other:?}"),
  };

  // Read-only open works.
  let ro_fh = match call(
    &fs,
    v1::session_request::Body::Open(v1::FsOpen {
      ino: seed.ino,
      flags: linux_flags::O_RDONLY,
      writeback: false,
    }),
  )
  .await
  {
    v1::session_response::Body::Opened(o) => o.fh,
    other => panic!("expected Opened, got {other:?}"),
  };

  // Mutating operations all return EROFS.
  expect_err(
    call(
      &fs,
      v1::session_request::Body::Open(v1::FsOpen {
        ino: seed.ino,
        flags: linux_flags::O_RDWR,
        writeback: false,
      }),
    )
    .await,
    errno::EROFS,
  );
  expect_err(
    call(
      &fs,
      v1::session_request::Body::Write(v1::FsWrite {
        ino: seed.ino,
        fh: ro_fh,
        offset: 0,
        data: b"nope".to_vec(),
      }),
    )
    .await,
    errno::EROFS,
  );
  expect_err(
    call(
      &fs,
      v1::session_request::Body::Create(v1::FsCreate {
        parent: ROOT_INO,
        name: "new.txt".into(),
        mode: 0o644,
        flags: linux_flags::O_WRONLY,
        writeback: false,
      }),
    )
    .await,
    errno::EROFS,
  );
  expect_err(
    call(
      &fs,
      v1::session_request::Body::CreateAndWrite(v1::FsCreateAndWrite {
        parent: ROOT_INO,
        name: "new.txt".into(),
        mode: 0o644,
        data: b"x".to_vec(),
      }),
    )
    .await,
    errno::EROFS,
  );
  expect_err(
    call(
      &fs,
      v1::session_request::Body::Mkdir(v1::FsMkdir {
        parent: ROOT_INO,
        name: "dir".into(),
        mode: 0o755,
      }),
    )
    .await,
    errno::EROFS,
  );
  expect_err(
    call(
      &fs,
      v1::session_request::Body::Unlink(v1::FsUnlink {
        parent: ROOT_INO,
        name: "seed.txt".into(),
      }),
    )
    .await,
    errno::EROFS,
  );
  expect_err(
    call(
      &fs,
      v1::session_request::Body::Rename(v1::FsRename {
        parent: ROOT_INO,
        name: "seed.txt".into(),
        new_parent: ROOT_INO,
        new_name: "moved.txt".into(),
        flags: 0,
      }),
    )
    .await,
    errno::EROFS,
  );
  expect_err(
    call(
      &fs,
      v1::session_request::Body::Setattr(v1::FsSetAttr {
        ino: seed.ino,
        fh: None,
        mode: Some(0o600),
        size: None,
        atime_sec: None,
        atime_nsec: None,
        mtime_sec: None,
        mtime_nsec: None,
      }),
    )
    .await,
    errno::EROFS,
  );

  let _ = call(
    &fs,
    v1::session_request::Body::Release(v1::FsRelease {
      ino: seed.ino,
      fh: ro_fh,
      lock_owner: 0,
    }),
  )
  .await;
}

#[tokio::test]
async fn forget_refcounting_evicts_at_zero() {
  let tmp = tempfile::tempdir().unwrap();
  std::fs::write(tmp.path().join("tracked.txt"), b"hi").unwrap();
  let fs = HostFs::open(tmp.path(), true, v1::FsLockMode::Local).unwrap();

  // Two lookups -> refcount == 2.
  let ino1 = match call(
    &fs,
    v1::session_request::Body::Lookup(v1::FsLookup {
      parent: ROOT_INO,
      name: "tracked.txt".into(),
    }),
  )
  .await
  {
    v1::session_response::Body::Entry(e) => e.attr.unwrap().ino,
    other => panic!("expected Entry, got {other:?}"),
  };
  let ino2 = match call(
    &fs,
    v1::session_request::Body::Lookup(v1::FsLookup {
      parent: ROOT_INO,
      name: "tracked.txt".into(),
    }),
  )
  .await
  {
    v1::session_response::Body::Entry(e) => e.attr.unwrap().ino,
    other => panic!("expected Entry, got {other:?}"),
  };
  assert_eq!(ino1, ino2);

  // Forget 1 -> still alive (refcount == 1).
  let _ = call(
    &fs,
    v1::session_request::Body::Forget(v1::FsForget {
      items: vec![v1::FsForgetItem {
        ino: ino1,
        nlookup: 1,
      }],
    }),
  )
  .await;
  assert!(matches!(
    call(
      &fs,
      v1::session_request::Body::Getattr(v1::FsGetAttr {
        ino: ino1,
        fh: None,
      }),
    )
    .await,
    v1::session_response::Body::Attr(_)
  ));

  // Forget 1 more -> refcount hits 0 and inode is evicted.
  let _ = call(
    &fs,
    v1::session_request::Body::Forget(v1::FsForget {
      items: vec![v1::FsForgetItem {
        ino: ino1,
        nlookup: 1,
      }],
    }),
  )
  .await;
  expect_err(
    call(
      &fs,
      v1::session_request::Body::Getattr(v1::FsGetAttr {
        ino: ino1,
        fh: None,
      }),
    )
    .await,
    errno::ENOENT,
  );

  // Root inode is never evicted even if forgotten.
  let _ = call(
    &fs,
    v1::session_request::Body::Forget(v1::FsForget {
      items: vec![v1::FsForgetItem {
        ino: ROOT_INO,
        nlookup: 99,
      }],
    }),
  )
  .await;
  assert!(matches!(
    call(
      &fs,
      v1::session_request::Body::Getattr(v1::FsGetAttr {
        ino: ROOT_INO,
        fh: None,
      }),
    )
    .await,
    v1::session_response::Body::Attr(_)
  ));
}

#[tokio::test]
async fn byte_range_lock_conflict_detection() {
  let tmp = tempfile::tempdir().unwrap();
  let fs = HostFs::open(tmp.path(), true, v1::FsLockMode::Remote).unwrap();

  let entry = match call(
    &fs,
    v1::session_request::Body::CreateAndWrite(v1::FsCreateAndWrite {
      parent: ROOT_INO,
      name: "locked.db".into(),
      mode: 0o644,
      data: vec![0u8; 4096],
    }),
  )
  .await
  {
    v1::session_response::Body::Entry(e) => e.attr.unwrap(),
    other => panic!("expected Entry, got {other:?}"),
  };

  let fh1 = match call(
    &fs,
    v1::session_request::Body::Open(v1::FsOpen {
      ino: entry.ino,
      flags: linux_flags::O_RDWR,
      writeback: false,
    }),
  )
  .await
  {
    v1::session_response::Body::Opened(o) => o.fh,
    other => panic!("expected Opened, got {other:?}"),
  };

  let fh2 = match call(
    &fs,
    v1::session_request::Body::Open(v1::FsOpen {
      ino: entry.ino,
      flags: linux_flags::O_RDWR,
      writeback: false,
    }),
  )
  .await
  {
    v1::session_response::Body::Opened(o) => o.fh,
    other => panic!("expected Opened, got {other:?}"),
  };

  // Owner 1 acquires an exclusive write lock on [0, 99].
  assert!(matches!(
    call(
      &fs,
      v1::session_request::Body::Setlock(v1::FsSetLock {
        ino: entry.ino,
        fh: fh1,
        lock_owner: 101,
        range: Some(v1::FsLockRange {
          start: 0,
          end: 99,
          kind: v1::FsLockType::Write as i32,
          pid: 1001,
        }),
        sleep: false,
      }),
    )
    .await,
    v1::session_response::Body::Ok(_)
  ));

  // Owner 2 probes [50, 150] -> reports Write conflict on [0, 99].
  let conflict = match call(
    &fs,
    v1::session_request::Body::Getlock(v1::FsGetLock {
      ino: entry.ino,
      fh: fh2,
      lock_owner: 202,
      range: Some(v1::FsLockRange {
        start: 50,
        end: 150,
        kind: v1::FsLockType::Read as i32,
        pid: 2002,
      }),
    }),
  )
  .await
  {
    v1::session_response::Body::Lock(l) => l.range.unwrap(),
    other => panic!("expected Lock, got {other:?}"),
  };
  assert_eq!(conflict.kind, v1::FsLockType::Write as i32);
  assert_eq!(conflict.start, 0);
  assert_eq!(conflict.end, 99);

  // Owner 2 non-blocking SetLock on overlapping range returns EAGAIN.
  expect_err(
    call(
      &fs,
      v1::session_request::Body::Setlock(v1::FsSetLock {
        ino: entry.ino,
        fh: fh2,
        lock_owner: 202,
        range: Some(v1::FsLockRange {
          start: 50,
          end: 150,
          kind: v1::FsLockType::Write as i32,
          pid: 2002,
        }),
        sleep: false,
      }),
    )
    .await,
    errno::EAGAIN,
  );

  // Non-overlapping range [200, 299] succeeds for Owner 2.
  assert!(matches!(
    call(
      &fs,
      v1::session_request::Body::Setlock(v1::FsSetLock {
        ino: entry.ino,
        fh: fh2,
        lock_owner: 202,
        range: Some(v1::FsLockRange {
          start: 200,
          end: 299,
          kind: v1::FsLockType::Write as i32,
          pid: 2002,
        }),
        sleep: false,
      }),
    )
    .await,
    v1::session_response::Body::Ok(_)
  ));

  // Owner 1 unlocks [0, 99]; Owner 2 can now acquire it.
  assert!(matches!(
    call(
      &fs,
      v1::session_request::Body::Setlock(v1::FsSetLock {
        ino: entry.ino,
        fh: fh1,
        lock_owner: 101,
        range: Some(v1::FsLockRange {
          start: 0,
          end: 99,
          kind: v1::FsLockType::Unlock as i32,
          pid: 1001,
        }),
        sleep: false,
      }),
    )
    .await,
    v1::session_response::Body::Ok(_)
  ));

  assert!(matches!(
    call(
      &fs,
      v1::session_request::Body::Setlock(v1::FsSetLock {
        ino: entry.ino,
        fh: fh2,
        lock_owner: 202,
        range: Some(v1::FsLockRange {
          start: 0,
          end: 99,
          kind: v1::FsLockType::Write as i32,
          pid: 2002,
        }),
        sleep: false,
      }),
    )
    .await,
    v1::session_response::Body::Ok(_)
  ));

  let _ = call(
    &fs,
    v1::session_request::Body::Release(v1::FsRelease {
      ino: entry.ino,
      fh: fh1,
      lock_owner: 101,
    }),
  )
  .await;
  let _ = call(
    &fs,
    v1::session_request::Body::Release(v1::FsRelease {
      ino: entry.ino,
      fh: fh2,
      lock_owner: 202,
    }),
  )
  .await;
}

#[tokio::test]
async fn serve_stream_mounts_and_handles_concurrent_requests() {
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path().to_path_buf();

  let (req_tx, req_rx) = mpsc::channel::<Result<v1::SessionRequest, String>>(16);
  let (resp_tx, mut resp_rx) = mpsc::channel::<v1::SessionResponse>(16);

  let server = tokio::spawn(async move {
    HostFs::serve_stream(ReceiverStream::new(req_rx), resp_tx, |mount| {
      let mode = v1::FsLockMode::try_from(mount.lock_mode).unwrap_or(v1::FsLockMode::Local);
      HostFs::open(&root, mount.writable, mode)
    })
    .await;
  });

  req_tx
    .send(Ok(v1::SessionRequest {
      id: 1,
      body: Some(v1::session_request::Body::Mount(v1::FsMount {
        executor: "native".into(),
        source: tmp.path().display().to_string(),
        target: "/workspace".into(),
        writable: true,
        lock_mode: v1::FsLockMode::Local as i32,
      })),
    }))
    .await
    .unwrap();

  let mounted = resp_rx.recv().await.unwrap();
  assert_eq!(mounted.id, 1);
  assert!(matches!(mounted.body, Some(v1::session_response::Body::Mounted(_))));

  // Send concurrent Ping + StatFs + CreateAndWrite requests.
  req_tx
    .send(Ok(v1::SessionRequest {
      id: 2,
      body: Some(v1::session_request::Body::Ping(v1::FsPing { timestamp_ms: 42 })),
    }))
    .await
    .unwrap();
  req_tx
    .send(Ok(v1::SessionRequest {
      id: 3,
      body: Some(v1::session_request::Body::Statfs(v1::FsStatFs { ino: ROOT_INO })),
    }))
    .await
    .unwrap();
  req_tx
    .send(Ok(v1::SessionRequest {
      id: 4,
      body: Some(v1::session_request::Body::CreateAndWrite(v1::FsCreateAndWrite {
        parent: ROOT_INO,
        name: "stream.txt".into(),
        mode: 0o644,
        data: b"streamed".to_vec(),
      })),
    }))
    .await
    .unwrap();
  drop(req_tx);

  let mut replies = HashMap::new();
  while let Some(r) = resp_rx.recv().await {
    replies.insert(r.id, r.body.unwrap());
  }
  server.await.unwrap();

  assert!(matches!(replies.get(&2), Some(v1::session_response::Body::Pong(v1::FsPong { timestamp_ms: 42 }))));
  assert!(matches!(replies.get(&3), Some(v1::session_response::Body::Statfs(_))));
  assert!(matches!(replies.get(&4), Some(v1::session_response::Body::Entry(_))));
}

#[cfg(windows)]
#[tokio::test]
async fn windows_posix_unlink_rename_case_aliasing_invalid_names_binary_and_advisory_locks() {
  let tmp = tempfile::tempdir().unwrap();
  let fs = HostFs::open(tmp.path(), true, v1::FsLockMode::Remote).unwrap();

  // 1. Unlink while open with O_RDONLY (was EPERM on sftp-server.exe).
  let ro_entry = match call(
    &fs,
    v1::session_request::Body::CreateAndWrite(v1::FsCreateAndWrite {
      parent: ROOT_INO,
      name: "unlink_ro.txt".into(),
      mode: 0o644,
      data: b"readonly open payload".to_vec(),
    }),
  )
  .await
  {
    v1::session_response::Body::Entry(e) => e.attr.unwrap(),
    other => panic!("expected Entry, got {other:?}"),
  };
  let ro_fh = match call(
    &fs,
    v1::session_request::Body::Open(v1::FsOpen {
      ino: ro_entry.ino,
      flags: linux_flags::O_RDONLY,
      writeback: false,
    }),
  )
  .await
  {
    v1::session_response::Body::Opened(o) => o.fh,
    other => panic!("expected Opened, got {other:?}"),
  };
  assert!(matches!(
    call(
      &fs,
      v1::session_request::Body::Unlink(v1::FsUnlink {
        parent: ROOT_INO,
        name: "unlink_ro.txt".into(),
      }),
    )
    .await,
    v1::session_response::Body::Ok(_)
  ));
  expect_err(
    call(
      &fs,
      v1::session_request::Body::Lookup(v1::FsLookup {
        parent: ROOT_INO,
        name: "unlink_ro.txt".into(),
      }),
    )
    .await,
    errno::ENOENT,
  );
  let ro_data = match call(
    &fs,
    v1::session_request::Body::Read(v1::FsRead {
      ino: ro_entry.ino,
      fh: ro_fh,
      offset: 0,
      size: 64,
    }),
  )
  .await
  {
    v1::session_response::Body::Data(d) => d.data,
    other => panic!("expected Data, got {other:?}"),
  };
  assert_eq!(ro_data, b"readonly open payload");
  let _ = call(
    &fs,
    v1::session_request::Body::Release(v1::FsRelease {
      ino: ro_entry.ino,
      fh: ro_fh,
      lock_owner: 0,
    }),
  )
  .await;

  // 2. Case-aliasing lookup (foo.txt vs FOO.TXT) returns the same NTFS FileId-backed inode.
  let foo = match call(
    &fs,
    v1::session_request::Body::CreateAndWrite(v1::FsCreateAndWrite {
      parent: ROOT_INO,
      name: "foo.txt".into(),
      mode: 0o644,
      data: b"case-alias".to_vec(),
    }),
  )
  .await
  {
    v1::session_response::Body::Entry(e) => e.attr.unwrap(),
    other => panic!("expected Entry, got {other:?}"),
  };
  let foo_upper = match call(
    &fs,
    v1::session_request::Body::Lookup(v1::FsLookup {
      parent: ROOT_INO,
      name: "FOO.TXT".into(),
    }),
  )
  .await
  {
    v1::session_response::Body::Entry(e) => e.attr.unwrap(),
    other => panic!("expected Entry, got {other:?}"),
  };
  assert_eq!(foo_upper.ino, foo.ino);
  assert_eq!(foo_upper.size, 10);

  // 3. Invalid Win32 filenames rejected with EINVAL (never ENOENT or silent NTFS ADS).
  for bad in ["quest?name.txt", "colon:name.txt", r"back\slash.txt"] {
    expect_err(
      call(
        &fs,
        v1::session_request::Body::Lookup(v1::FsLookup {
          parent: ROOT_INO,
          name: bad.into(),
        }),
      )
      .await,
      errno::EINVAL,
    );
    expect_err(
      call(
        &fs,
        v1::session_request::Body::CreateAndWrite(v1::FsCreateAndWrite {
          parent: ROOT_INO,
          name: bad.into(),
          mode: 0o644,
          data: b"nope".to_vec(),
        }),
      )
      .await,
      errno::EINVAL,
    );
  }
  assert!(!tmp.path().join("colon").exists());

  // 4. Binary fidelity (all 256 byte values + CRLF/LF/NUL preserved verbatim).
  let payload: Vec<u8> = (0u8..=255u8).chain(*b"\r\n\n\r\0binary\x00end").collect();
  let bin = match call(
    &fs,
    v1::session_request::Body::CreateAndWrite(v1::FsCreateAndWrite {
      parent: ROOT_INO,
      name: "binary.dat".into(),
      mode: 0o644,
      data: payload.clone(),
    }),
  )
  .await
  {
    v1::session_response::Body::Entry(e) => e.attr.unwrap(),
    other => panic!("expected Entry, got {other:?}"),
  };
  assert_eq!(bin.size, payload.len() as u64);
  let bin_fh = match call(
    &fs,
    v1::session_request::Body::Open(v1::FsOpen {
      ino: bin.ino,
      flags: linux_flags::O_RDONLY,
      writeback: false,
    }),
  )
  .await
  {
    v1::session_response::Body::Opened(o) => o.fh,
    other => panic!("expected Opened, got {other:?}"),
  };
  let bin_read = match call(
    &fs,
    v1::session_request::Body::Read(v1::FsRead {
      ino: bin.ino,
      fh: bin_fh,
      offset: 0,
      size: (payload.len() + 16) as u32,
    }),
  )
  .await
  {
    v1::session_response::Body::Data(d) => d.data,
    other => panic!("expected Data, got {other:?}"),
  };
  assert_eq!(bin_read, payload);
  assert_eq!(std::fs::read(tmp.path().join("binary.dat")).unwrap(), payload);
  let _ = call(
    &fs,
    v1::session_request::Body::Release(v1::FsRelease {
      ino: bin.ino,
      fh: bin_fh,
      lock_owner: 0,
    }),
  )
  .await;

  // 5. Advisory byte-range lock does not block host readers (no mandatory LockFileEx).
  let lock_fh = match call(
    &fs,
    v1::session_request::Body::Open(v1::FsOpen {
      ino: foo.ino,
      flags: linux_flags::O_RDWR,
      writeback: false,
    }),
  )
  .await
  {
    v1::session_response::Body::Opened(o) => o.fh,
    other => panic!("expected Opened, got {other:?}"),
  };
  assert!(matches!(
    call(
      &fs,
      v1::session_request::Body::Setlock(v1::FsSetLock {
        ino: foo.ino,
        fh: lock_fh,
        lock_owner: 77,
        range: Some(v1::FsLockRange {
          start: 0,
          end: 100,
          kind: v1::FsLockType::Write as i32,
          pid: 777,
        }),
        sleep: false,
      }),
    )
    .await,
    v1::session_response::Body::Ok(_)
  ));
  assert_eq!(std::fs::read(tmp.path().join("foo.txt")).unwrap(), b"case-alias");
  let _ = call(
    &fs,
    v1::session_request::Body::Release(v1::FsRelease {
      ino: foo.ino,
      fh: lock_fh,
      lock_owner: 77,
    }),
  )
  .await;
}
