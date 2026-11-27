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

async fn trait_rpc(
  tx: &tokio::sync::mpsc::Sender<SessionRequest>,
  rx: &mut needy_api_server::control::ToRunner<SessionResponse>,
  id: u64,
  body: session_request::Body,
) -> session_response::Body {
  use futures_util::StreamExt as _;
  tx.send(SessionRequest {
    id,
    body: Some(body),
  })
  .await
  .unwrap();
  let resp = rx.next().await.expect("expected session response");
  assert_eq!(resp.id, id);
  resp.body.expect("expected response body")
}

// ROOT CAUSE:
//
// `crates/daemon/src/fs.rs` forwarded `FsService` RPCs directly to `HostFs` or
// a remote runner without evaluating the run's `file.open` chaos faults,
// because `FsOpen` identifies files by inode rather than path.
//
// Before the fix, `chaos/fault=file` injected 0 faults on `FsService` mounts.
// The fix tracks `ino -> actor_relative_path` per session, evaluates
// `run.injector` on `FsOpen`/`FsCreate`/`FsCreateAndWrite`, returns `FsError`
// (or delays) when a fault fires, and emits `fault.injected` in the run trace.
#[tokio::test]
async fn file_fault_eio_on_fs_open_and_create_emits_trace_event() {
  use futures_util::StreamExt as _;
  use needy_api_client::proto::v1::{FsCreate, FsMkdir};
  use needy_api_server::control::{Caller, Fs, Runs};
  use needy_config::profile::FaultDomain;
  use needy_core::event::Event;
  use tokio_stream::wrappers::ReceiverStream;

  let started = testing::daemon().await;
  let workspace = tempfile::tempdir().unwrap();
  std::fs::write(workspace.path().join("broken.txt"), b"should fail").unwrap();
  std::fs::write(workspace.path().join("ok.txt"), b"hello ok").unwrap();

  let profile = r#"
actor: { driver: linux-firecracker }
executors:
  here: { driver: native }
chaos:
  faults:
    broken: { file: { open: "**/broken.txt" }, errno: EIO }
"#;
  let created = Runs::create(&started.daemon, testing::new_run(profile, started.dir.path())).await.unwrap();

  let (req_tx, req_rx) = tokio::sync::mpsc::channel::<SessionRequest>(32);
  let incoming = Box::pin(ReceiverStream::new(req_rx).map(Ok::<_, tonic::Status>));
  let mut rx = Fs::session(
    &started.daemon,
    Caller::Actor {
      run: created.id.clone(),
    },
    incoming,
  )
  .await
  .unwrap();

  match trait_rpc(
    &req_tx,
    &mut rx,
    1,
    session_request::Body::Mount(FsMount {
      executor: "here".into(),
      source: workspace.path().to_string_lossy().into_owned(),
      target: "/workspace".into(),
      writable: true,
      lock_mode: FsLockMode::Local as i32,
    }),
  )
  .await
  {
    session_response::Body::Mounted(m) => assert_eq!(m.root.unwrap().ino, ROOT_INO),
    other => panic!("expected Mounted, got {other:?}"),
  }

  let broken_ino = match trait_rpc(
    &req_tx,
    &mut rx,
    2,
    session_request::Body::Lookup(FsLookup {
      parent: ROOT_INO,
      name: "broken.txt".into(),
    }),
  )
  .await
  {
    session_response::Body::Entry(e) => e.attr.unwrap().ino,
    other => panic!("expected Entry, got {other:?}"),
  };

  let ok_ino = match trait_rpc(
    &req_tx,
    &mut rx,
    3,
    session_request::Body::Lookup(FsLookup {
      parent: ROOT_INO,
      name: "ok.txt".into(),
    }),
  )
  .await
  {
    session_response::Body::Entry(e) => e.attr.unwrap().ino,
    other => panic!("expected Entry, got {other:?}"),
  };

  // Matching path returns EIO without opening the file.
  match trait_rpc(
    &req_tx,
    &mut rx,
    4,
    session_request::Body::Open(FsOpen {
      ino: broken_ino,
      flags: linux_flags::O_RDONLY,
      writeback: false,
    }),
  )
  .await
  {
    session_response::Body::Error(err) => assert_eq!(err.errno, errno::EIO),
    other => panic!("expected Error(EIO), got {other:?}"),
  }

  // Non-matching path opens and reads normally.
  let fh = match trait_rpc(
    &req_tx,
    &mut rx,
    5,
    session_request::Body::Open(FsOpen {
      ino: ok_ino,
      flags: linux_flags::O_RDONLY,
      writeback: false,
    }),
  )
  .await
  {
    session_response::Body::Opened(o) => o.fh,
    other => panic!("expected Opened, got {other:?}"),
  };
  match trait_rpc(
    &req_tx,
    &mut rx,
    6,
    session_request::Body::Read(FsRead {
      ino: ok_ino,
      fh,
      offset: 0,
      size: 64,
    }),
  )
  .await
  {
    session_response::Body::Data(d) => assert_eq!(d.data, b"hello ok"),
    other => panic!("expected Data, got {other:?}"),
  }

  // Subdirectory FsCreate and FsCreateAndWrite matching the glob also return EIO
  // and do not create the file on disk.
  let sub_ino = match trait_rpc(
    &req_tx,
    &mut rx,
    7,
    session_request::Body::Mkdir(FsMkdir {
      parent: ROOT_INO,
      name: "sub".into(),
      mode: 0o755,
    }),
  )
  .await
  {
    session_response::Body::Entry(e) => e.attr.unwrap().ino,
    other => panic!("expected Entry for mkdir, got {other:?}"),
  };
  match trait_rpc(
    &req_tx,
    &mut rx,
    8,
    session_request::Body::Create(FsCreate {
      parent: sub_ino,
      name: "broken.txt".into(),
      mode: 0o644,
      flags: linux_flags::O_WRONLY | linux_flags::O_CREAT,
      writeback: false,
    }),
  )
  .await
  {
    session_response::Body::Error(err) => assert_eq!(err.errno, errno::EIO),
    other => panic!("expected Error(EIO) on create, got {other:?}"),
  }
  match trait_rpc(
    &req_tx,
    &mut rx,
    9,
    session_request::Body::CreateAndWrite(FsCreateAndWrite {
      parent: sub_ino,
      name: "broken.txt".into(),
      mode: 0o644,
      data: b"nope".to_vec(),
    }),
  )
  .await
  {
    session_response::Body::Error(err) => assert_eq!(err.errno, errno::EIO),
    other => panic!("expected Error(EIO) on create_and_write, got {other:?}"),
  }
  assert!(!workspace.path().join("sub/broken.txt").exists());

  drop(req_tx);

  let stored = Runs::events(&started.daemon, &created.id).await.unwrap().events;
  let injected: Vec<_> = stored
    .iter()
    .filter_map(|e| match Event::decode(&e.kind, e.version, &e.payload).ok()? {
      Event::FaultInjected(fired) => Some(fired),
      _ => None,
    })
    .collect();
  assert_eq!(injected.len(), 3, "expected 3 fault.injected events, got {injected:?}");
  assert_eq!(injected[0].domain, FaultDomain::File);
  assert_eq!(injected[0].name, "broken");
  assert_eq!(injected[0].target, "/workspace/broken.txt");
  assert_eq!(injected[0].effect.errno.as_deref(), Some("EIO"));
  assert_eq!(injected[1].target, "/workspace/sub/broken.txt");
  assert_eq!(injected[2].target, "/workspace/sub/broken.txt");
}

#[tokio::test]
async fn file_fault_tracks_rename_and_delay() {
  use std::time::{Duration, Instant};

  use futures_util::StreamExt as _;
  use needy_api_client::proto::v1::{FsForget, FsForgetItem, FsRename};
  use needy_api_server::control::{Caller, Fs, Runs};
  use needy_config::profile::FaultDomain;
  use needy_core::event::Event;
  use tokio_stream::wrappers::ReceiverStream;

  let started = testing::daemon().await;
  let workspace = tempfile::tempdir().unwrap();
  std::fs::write(workspace.path().join("orig.txt"), b"renamed payload").unwrap();

  let profile = r#"
actor: { driver: linux-firecracker }
executors:
  here: { driver: native }
chaos:
  faults:
    slow-moved: { file: { open: "/workspace/moved.txt" }, delay: 50ms }
"#;
  let created = Runs::create(&started.daemon, testing::new_run(profile, started.dir.path())).await.unwrap();

  let (req_tx, req_rx) = tokio::sync::mpsc::channel::<SessionRequest>(32);
  let incoming = Box::pin(ReceiverStream::new(req_rx).map(Ok::<_, tonic::Status>));
  let mut rx = Fs::session(
    &started.daemon,
    Caller::Actor {
      run: created.id.clone(),
    },
    incoming,
  )
  .await
  .unwrap();

  match trait_rpc(
    &req_tx,
    &mut rx,
    1,
    session_request::Body::Mount(FsMount {
      executor: "here".into(),
      source: workspace.path().to_string_lossy().into_owned(),
      target: "/workspace".into(),
      writable: true,
      lock_mode: FsLockMode::Local as i32,
    }),
  )
  .await
  {
    session_response::Body::Mounted(_) => {}
    other => panic!("expected Mounted, got {other:?}"),
  }

  let orig_ino = match trait_rpc(
    &req_tx,
    &mut rx,
    2,
    session_request::Body::Lookup(FsLookup {
      parent: ROOT_INO,
      name: "orig.txt".into(),
    }),
  )
  .await
  {
    session_response::Body::Entry(e) => e.attr.unwrap().ino,
    other => panic!("expected Entry, got {other:?}"),
  };

  match trait_rpc(
    &req_tx,
    &mut rx,
    3,
    session_request::Body::Rename(FsRename {
      parent: ROOT_INO,
      name: "orig.txt".into(),
      new_parent: ROOT_INO,
      new_name: "moved.txt".into(),
      flags: 0,
    }),
  )
  .await
  {
    session_response::Body::Ok(_) => {}
    other => panic!("expected Ok for rename, got {other:?}"),
  }

  let start = Instant::now();
  match trait_rpc(
    &req_tx,
    &mut rx,
    4,
    session_request::Body::Open(FsOpen {
      ino: orig_ino,
      flags: linux_flags::O_RDONLY,
      writeback: false,
    }),
  )
  .await
  {
    session_response::Body::Opened(_) => {}
    other => panic!("expected Opened after delay, got {other:?}"),
  }
  assert!(start.elapsed() >= Duration::from_millis(40), "expected >=40ms delay, got {:?}", start.elapsed());

  // Forget cleans up the tracked inode without error.
  match trait_rpc(
    &req_tx,
    &mut rx,
    5,
    session_request::Body::Forget(FsForget {
      items: vec![FsForgetItem {
        ino: orig_ino,
        nlookup: 1,
      }],
    }),
  )
  .await
  {
    session_response::Body::Ok(_) => {}
    other => panic!("expected Ok for forget, got {other:?}"),
  }

  drop(req_tx);

  let stored = Runs::events(&started.daemon, &created.id).await.unwrap().events;
  let injected: Vec<_> = stored
    .iter()
    .filter_map(|e| match Event::decode(&e.kind, e.version, &e.payload).ok()? {
      Event::FaultInjected(fired) => Some(fired),
      _ => None,
    })
    .collect();
  assert_eq!(injected.len(), 1, "expected 1 fault.injected event, got {injected:?}");
  assert_eq!(injected[0].domain, FaultDomain::File);
  assert_eq!(injected[0].name, "slow-moved");
  assert_eq!(injected[0].target, "/workspace/moved.txt");
  assert_eq!(injected[0].effect.delay, Some(Duration::from_millis(50)));
}
