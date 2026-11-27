use needy_api_client::proto::v1::{
  FsCreateAndWrite, FsLockMode, FsLookup, FsMount, FsOpen, FsPing, FsRead, FsReadDirPlus, FsRelease, FsUnlink, SessionRequest,
  SessionResponse, session_request, session_response,
};
use needy_core_executor::fs::{ROOT_INO, errno, linux_flags};

use crate::testing;

async fn rpc(
  tx: &tokio::sync::mpsc::Sender<SessionRequest>,
  rx: &mut tonic::Streaming<SessionResponse>,
  id: u64,
  body: session_request::Body,
) -> session_response::Body {
  tx.send(SessionRequest {
    id,
    body: Some(body),
  })
  .await
  .unwrap();
  let resp = rx.message().await.unwrap().expect("expected session response");
  assert_eq!(resp.id, id);
  resp.body.expect("expected response body")
}

#[tokio::test]
async fn local_fs_session_roundtrip() {
  let served = testing::serve(testing::daemon().await, Some("secret")).await;
  let client = served.client.clone().with_token("secret").unwrap();
  let workspace = tempfile::tempdir().unwrap();

  let (tx, mut rx) = client.fs_session().await.unwrap();

  // Mount local directory.
  let mounted = rpc(
    &tx,
    &mut rx,
    1,
    session_request::Body::Mount(FsMount {
      executor: "local".into(),
      source: workspace.path().to_string_lossy().into_owned(),
      target: "/workspace".into(),
      writable: true,
      lock_mode: FsLockMode::Local as i32,
    }),
  )
  .await;
  match mounted {
    session_response::Body::Mounted(m) => {
      assert!(m.writable);
      assert_eq!(m.root.unwrap().ino, ROOT_INO);
    }
    other => panic!("expected Mounted, got {other:?}"),
  }

  // CreateAndWrite a file.
  let created = rpc(
    &tx,
    &mut rx,
    2,
    session_request::Body::CreateAndWrite(FsCreateAndWrite {
      parent: ROOT_INO,
      name: "hello.txt".into(),
      mode: 0o644,
      data: b"hello local fs".to_vec(),
    }),
  )
  .await;
  let file_ino = match created {
    session_response::Body::Entry(e) => {
      let attr = e.attr.unwrap();
      assert_eq!(attr.size, 14);
      attr.ino
    }
    other => panic!("expected Entry, got {other:?}"),
  };
  assert_eq!(std::fs::read(workspace.path().join("hello.txt")).unwrap(), b"hello local fs");

  // ReadDirPlus lists the created file.
  let listing = rpc(
    &tx,
    &mut rx,
    3,
    session_request::Body::Readdirplus(FsReadDirPlus {
      ino: ROOT_INO,
      offset: 0,
      max_entries: 64,
    }),
  )
  .await;
  match listing {
    session_response::Body::Readdirplus(r) => {
      assert!(r.eof);
      let entry = r.entries.iter().find(|e| e.name == "hello.txt").expect("hello.txt in readdirplus");
      assert_eq!(entry.ino, file_ino);
    }
    other => panic!("expected Readdirplus, got {other:?}"),
  }

  // Open, Read, Release.
  let fh = match rpc(
    &tx,
    &mut rx,
    4,
    session_request::Body::Open(FsOpen {
      ino: file_ino,
      flags: linux_flags::O_RDONLY,
      writeback: false,
    }),
  )
  .await
  {
    session_response::Body::Opened(o) => o.fh,
    other => panic!("expected Opened, got {other:?}"),
  };

  match rpc(
    &tx,
    &mut rx,
    5,
    session_request::Body::Read(FsRead {
      ino: file_ino,
      fh,
      offset: 0,
      size: 64,
    }),
  )
  .await
  {
    session_response::Body::Data(d) => {
      assert_eq!(d.data, b"hello local fs");
    }
    other => panic!("expected Data, got {other:?}"),
  }

  match rpc(
    &tx,
    &mut rx,
    6,
    session_request::Body::Release(FsRelease {
      ino: file_ino,
      fh,
      lock_owner: 0,
    }),
  )
  .await
  {
    session_response::Body::Ok(_) => {}
    other => panic!("expected Ok, got {other:?}"),
  }

  // Unlink removes the file from disk.
  match rpc(
    &tx,
    &mut rx,
    7,
    session_request::Body::Unlink(FsUnlink {
      parent: ROOT_INO,
      name: "hello.txt".into(),
    }),
  )
  .await
  {
    session_response::Body::Ok(_) => {}
    other => panic!("expected Ok, got {other:?}"),
  }
  assert!(!workspace.path().join("hello.txt").exists());

  drop(tx);
  assert!(rx.message().await.unwrap().is_none());
}

#[tokio::test]
async fn remote_runner_fs_task_roundtrip() {
  let served = testing::serve(testing::daemon().await, Some("secret")).await;
  let operator = served.client.clone().with_token("secret").unwrap();
  let _runner = testing::join_runner(&served, &operator, "worker-1", "executor(native)", &[]).await;

  let workspace = tempfile::tempdir().unwrap();
  std::fs::write(workspace.path().join("seed.txt"), b"from runner").unwrap();

  let (tx, mut rx) = operator.fs_session().await.unwrap();

  // Mount naming the connected runner triggers RunnerService.FsTask dispatch.
  match rpc(
    &tx,
    &mut rx,
    1,
    session_request::Body::Mount(FsMount {
      executor: "worker-1".into(),
      source: workspace.path().to_string_lossy().into_owned(),
      target: "/workspace".into(),
      writable: true,
      lock_mode: FsLockMode::Local as i32,
    }),
  )
  .await
  {
    session_response::Body::Mounted(m) => {
      assert!(m.writable);
      assert_eq!(m.root.unwrap().ino, ROOT_INO);
    }
    other => panic!("expected Mounted, got {other:?}"),
  }

  // Lookup and read pre-existing file served by the runner's HostFs.
  let seed_ino = match rpc(
    &tx,
    &mut rx,
    2,
    session_request::Body::Lookup(FsLookup {
      parent: ROOT_INO,
      name: "seed.txt".into(),
    }),
  )
  .await
  {
    session_response::Body::Entry(e) => e.attr.unwrap().ino,
    other => panic!("expected Entry, got {other:?}"),
  };

  let fh = match rpc(
    &tx,
    &mut rx,
    3,
    session_request::Body::Open(FsOpen {
      ino: seed_ino,
      flags: linux_flags::O_RDONLY,
      writeback: false,
    }),
  )
  .await
  {
    session_response::Body::Opened(o) => o.fh,
    other => panic!("expected Opened, got {other:?}"),
  };

  match rpc(
    &tx,
    &mut rx,
    4,
    session_request::Body::Read(FsRead {
      ino: seed_ino,
      fh,
      offset: 0,
      size: 64,
    }),
  )
  .await
  {
    session_response::Body::Data(d) => assert_eq!(d.data, b"from runner"),
    other => panic!("expected Data, got {other:?}"),
  }

  match rpc(
    &tx,
    &mut rx,
    5,
    session_request::Body::Release(FsRelease {
      ino: seed_ino,
      fh,
      lock_owner: 0,
    }),
  )
  .await
  {
    session_response::Body::Ok(_) => {}
    other => panic!("expected Ok, got {other:?}"),
  }

  // CreateAndWrite through the runner writes to disk.
  match rpc(
    &tx,
    &mut rx,
    6,
    session_request::Body::CreateAndWrite(FsCreateAndWrite {
      parent: ROOT_INO,
      name: "created.txt".into(),
      mode: 0o644,
      data: b"via runner fs_task".to_vec(),
    }),
  )
  .await
  {
    session_response::Body::Entry(e) => assert_eq!(e.attr.unwrap().size, 18),
    other => panic!("expected Entry, got {other:?}"),
  }
  assert_eq!(std::fs::read(workspace.path().join("created.txt")).unwrap(), b"via runner fs_task");

  // ReadDirPlus returns both files.
  match rpc(
    &tx,
    &mut rx,
    7,
    session_request::Body::Readdirplus(FsReadDirPlus {
      ino: ROOT_INO,
      offset: 0,
      max_entries: 64,
    }),
  )
  .await
  {
    session_response::Body::Readdirplus(r) => {
      let mut names: Vec<_> = r.entries.iter().map(|e| e.name.as_str()).collect();
      names.sort();
      assert_eq!(names, vec!["created.txt", "seed.txt"]);
    }
    other => panic!("expected Readdirplus, got {other:?}"),
  }

  // Unlink removes created.txt.
  match rpc(
    &tx,
    &mut rx,
    8,
    session_request::Body::Unlink(FsUnlink {
      parent: ROOT_INO,
      name: "created.txt".into(),
    }),
  )
  .await
  {
    session_response::Body::Ok(_) => {}
    other => panic!("expected Ok, got {other:?}"),
  }
  assert!(!workspace.path().join("created.txt").exists());

  drop(tx);
  assert!(rx.message().await.unwrap().is_none());
}

#[tokio::test]
async fn first_request_must_be_mount() {
  let served = testing::serve(testing::daemon().await, Some("secret")).await;
  let client = served.client.clone().with_token("secret").unwrap();

  let (tx, mut rx) = client.fs_session().await.unwrap();
  match rpc(&tx, &mut rx, 9, session_request::Body::Ping(FsPing { timestamp_ms: 42 })).await {
    session_response::Body::Error(err) => assert_eq!(err.errno, errno::EINVAL),
    other => panic!("expected Error(EINVAL), got {other:?}"),
  }
}
