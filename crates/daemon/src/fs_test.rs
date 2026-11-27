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

#[tokio::test]
async fn executor_server_serves_fs_session_roundtrip() {
  use std::sync::Arc;

  use needy_api_server::control::Caller;
  use needy_api_server::{Health, Listen, Server};
  use needy_core_executor::process::Host;
  use needy_core_executor::service::ExecutorService;
  use tokio_util::sync::CancellationToken;
  use tokio_util::task::TaskTracker;

  let dir = tempfile::Builder::new().prefix("exec-fs-").tempdir_in("/tmp").unwrap();
  let socket = dir.path().join("executor.sock");
  let workspace = tempfile::tempdir().unwrap();
  std::fs::write(workspace.path().join("seed.txt"), b"from executor").unwrap();

  let tasks = TaskTracker::new();
  let cancel = CancellationToken::new();
  let service = ExecutorService {
    host: Host::default(),
    name: "container-exec".into(),
  };
  let bound = Server::new(
    Health {
      instance_id: "exec-fs-test".into(),
      role: "executor",
    },
    Caller::Operator,
  )
  .process_service(service)
  .fs(Arc::new(crate::executor::ExecutorFs::new(tasks.clone(), cancel.clone())))
  .bind(&[Listen::Unix(socket.clone())])
  .await
  .unwrap();
  let stop = cancel.clone();
  let served = tokio::spawn(async move { bound.serve(stop).await });

  let client = needy_api_client::Client::connect(&format!("unix:{}", socket.display())).await.unwrap();
  let (tx, mut rx) = client.fs_session().await.unwrap();

  match rpc(
    &tx,
    &mut rx,
    1,
    session_request::Body::Mount(FsMount {
      executor: "build".into(),
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
    session_response::Body::Data(d) => assert_eq!(d.data, b"from executor"),
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

  drop(tx);
  assert!(rx.message().await.unwrap().is_none());

  cancel.cancel();
  tasks.close();
  tasks.wait().await;
  served.await.unwrap().unwrap();
}

// ROOT CAUSE:
//
// If a run uses a `linux-container` executor, its workspace path (e.g.
// `/workspace`) exists inside the executor container's mount namespace rather
// than on the control host.
//
// Before the fix, `needy daemon executor` did not serve `FsService`, and
// `Daemon::resolve_fs_target` fell back to local `HostFs::open(&mount.source)`
// for any non-runner executor, failing with `ENOENT`.
// The fix serves `FsService` on the executor server and forwards `FsService`
// sessions for `linux-container` executors over the container's executor client.
#[tokio::test]
async fn linux_container_executor_fs_session_forwards_to_executor_server() {
  use std::collections::HashMap;
  use std::path::PathBuf;
  use std::sync::{Arc, Mutex};

  use futures_util::StreamExt as _;
  use needy_api_server::control::{Caller, ControlError, FromRunner, Fs, Runs, StopMode, ToRunner};
  use needy_api_server::{Health, Listen, Server};
  use needy_core::types::Principal;
  use needy_core_executor::driver::{LinuxContainer, Native, linux_container};
  use needy_core_executor::engine::{self, testing::Fake};
  use needy_core_executor::process::Host;
  use needy_core_executor::service::ExecutorService;
  use tokio_util::sync::CancellationToken;
  use tokio_util::task::TaskTracker;

  /// Stands in for the container's mount namespace: maps a container-only
  /// path (`from`) that does not exist on the host to a real host `TempDir`
  /// (`to`) before handing the stream to `ExecutorFs`.
  struct ContainerNamespaceFs {
    from: String,
    to: PathBuf,
    inner: crate::executor::ExecutorFs,
  }

  #[async_trait::async_trait]
  impl Fs for ContainerNamespaceFs {
    async fn session(&self, caller: Caller, requests: FromRunner<SessionRequest>) -> Result<ToRunner<SessionResponse>, ControlError> {
      let from = self.from.clone();
      let to = self.to.to_string_lossy().into_owned();
      let mapped = requests.map(move |item| {
        item.map(|mut req| {
          if let Some(session_request::Body::Mount(ref mut mount)) = req.body
            && mount.source == from
          {
            mount.source = to.clone();
          }
          req
        })
      });
      self.inner.session(caller, Box::pin(mapped)).await
    }
  }

  let container_path = "/needy-test-container-only-workspace";
  assert!(!std::path::Path::new(container_path).exists(), "container-only path must not exist on host");

  let container_ws = tempfile::tempdir().unwrap();
  std::fs::write(container_ws.path().join("inside.txt"), b"inside container").unwrap();

  type RunningExecutor = (CancellationToken, TaskTracker, tokio::task::JoinHandle<()>);
  let running: Arc<Mutex<HashMap<String, RunningExecutor>>> = Arc::default();
  let start_running = running.clone();
  let remove_running = running.clone();
  let ws_path = container_ws.path().to_path_buf();
  let fake_engine = Arc::new(
    Fake::new()
      .with_stop_removes()
      .with_wait_error(engine::Error::Refused("executor fixture only".into()))
      .with_on_start(Arc::new(move |name, body| {
        let running = start_running.clone();
        let ws_path = ws_path.clone();
        let name = name.to_string();
        Box::pin(async move {
          let body = body.unwrap();
          let cmd = body.cmd.unwrap();
          let endpoint = cmd.windows(2).find(|pair| pair[0] == "--listen").unwrap()[1].strip_prefix("unix:").unwrap();
          let endpoint = std::path::Path::new(endpoint);
          let target = endpoint.parent().unwrap().to_str().unwrap();
          let binds = body.host_config.unwrap().binds.unwrap();
          let source = binds
            .iter()
            .find_map(|bind| {
              let (source, to) = bind.rsplit_once(':')?;
              (to == target).then_some(source)
            })
            .unwrap();
          let socket = PathBuf::from(source).join(endpoint.file_name().unwrap());
          let tasks = TaskTracker::new();
          let cancel = CancellationToken::new();
          let service = ExecutorService {
            host: Host::default(),
            name: name.clone(),
          };
          let fs = Arc::new(ContainerNamespaceFs {
            from: container_path.into(),
            to: ws_path,
            inner: crate::executor::ExecutorFs::new(tasks.clone(), cancel.clone()),
          });
          let bound = Server::new(
            Health {
              instance_id: name.clone(),
              role: "executor",
            },
            Caller::Operator,
          )
          .process_service(service)
          .fs(fs)
          .bind(&[Listen::Unix(socket)])
          .await
          .map_err(|e| engine::Error::Refused(e.to_string()))?;
          let stop = cancel.clone();
          let served = tokio::spawn(async move {
            let _ = bound.serve(stop).await;
          });
          running.lock().unwrap().insert(name, (cancel, tasks, served));
          Ok(std::process::id())
        })
      }))
      .with_on_remove(Arc::new(move |name| {
        let running = remove_running.clone();
        let name = name.to_string();
        Box::pin(async move {
          let entry = running.lock().unwrap().remove(&name);
          if let Some((cancel, tasks, served)) = entry {
            cancel.cancel();
            tasks.close();
            tasks.wait().await;
            let _ = served.await;
          }
          Ok(())
        })
      })),
  );

  let dir = tempfile::Builder::new().prefix("fs-ctr-").tempdir_in("/tmp").unwrap();
  let started = testing::daemon_in(dir, |_| {}).await;
  let local_endpoint = format!("unix:{}", started.daemon.config.runtime_dir.join("local.sock").display());
  let native = Arc::new(Native::new(needy_api_client::Client::connect(&local_endpoint).await.unwrap()));
  let container = LinuxContainer::with_engine(
    linux_container::Options {
      image: started.daemon.config.executor_image.clone(),
      state_dir: started.daemon.config.state_dir.clone(),
      allow: Vec::new(),
    },
    fake_engine,
  );
  let links: Arc<crate::link::Links> = Arc::default();
  let cancel = CancellationToken::new();
  let daemon = crate::Daemon {
    pool: Arc::new(crate::pool::Pool::new(native, Some(container), links.clone())),
    runs: Arc::new(crate::run::Registry::new(started.daemon.state.clone())),
    processes: Arc::default(),
    links,
    runner_hosts: Arc::default(),
    tasks: TaskTracker::new(),
    creates: TaskTracker::new(),
    outside_runs: cancel.child_token(),
    cancel,
    shutting_down: Arc::default(),
    suspending: Arc::default(),
    runs_stopped: Arc::default(),
    cleanup_incomplete: Arc::default(),
    shutdown_started: Arc::default(),
    ..started.daemon.clone()
  };

  let uid = unsafe { libc::geteuid() };
  let gid = unsafe { libc::getegid() };
  let profile = format!(
    "actor: {{ driver: linux-firecracker }}\nuser: {{ name: user, uid: {uid}, gid: {gid} }}\nexecutors:\n  build: {{ driver: linux-container }}\ncommands:\n  - {{ match: '**', to: build }}\n"
  );
  let run_info = Runs::create(&daemon, testing::new_run(&profile, started.dir.path())).await.unwrap();

  let control_socket = started.dir.path().join("control-fs.sock");
  let control_bound = daemon.server(Some("secret".into())).bind(&[Listen::Unix(control_socket.clone())]).await.unwrap();
  let actor_socket = started.dir.path().join("actor-fs.sock");
  let actor_bound = Server::new(
    Health {
      instance_id: run_info.id.clone(),
      role: "guest",
    },
    Caller::Actor {
      run: run_info.id.clone(),
    },
  )
  .fs(Arc::new(daemon.clone()))
  .bind(&[Listen::Unix(actor_socket.clone())])
  .await
  .unwrap();
  let server_stop = CancellationToken::new();
  let _guard = server_stop.clone().drop_guard();
  tokio::spawn(control_bound.serve(server_stop.clone()));
  tokio::spawn(actor_bound.serve(server_stop));

  // Verify both the run-scoped actor path (`run.executor("build")`) and the
  // operator pool path (`pool.named("build")`) forward to the container's
  // executor `FsService`.
  let actor_client = needy_api_client::Client::connect(&format!("unix:{}", actor_socket.display())).await.unwrap();
  let (tx, mut rx) = actor_client.fs_session().await.unwrap();

  match rpc(
    &tx,
    &mut rx,
    1,
    session_request::Body::Mount(FsMount {
      executor: "build".into(),
      source: container_path.into(),
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
    other => panic!("expected Mounted from container executor, got {other:?}"),
  }

  let inside_ino = match rpc(
    &tx,
    &mut rx,
    2,
    session_request::Body::Lookup(FsLookup {
      parent: ROOT_INO,
      name: "inside.txt".into(),
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
      ino: inside_ino,
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
      ino: inside_ino,
      fh,
      offset: 0,
      size: 64,
    }),
  )
  .await
  {
    session_response::Body::Data(d) => assert_eq!(d.data, b"inside container"),
    other => panic!("expected Data, got {other:?}"),
  }

  match rpc(
    &tx,
    &mut rx,
    5,
    session_request::Body::Release(FsRelease {
      ino: inside_ino,
      fh,
      lock_owner: 0,
    }),
  )
  .await
  {
    session_response::Body::Ok(_) => {}
    other => panic!("expected Ok, got {other:?}"),
  }

  match rpc(
    &tx,
    &mut rx,
    6,
    session_request::Body::CreateAndWrite(FsCreateAndWrite {
      parent: ROOT_INO,
      name: "written.txt".into(),
      mode: 0o644,
      data: b"via container fs".to_vec(),
    }),
  )
  .await
  {
    session_response::Body::Entry(e) => assert_eq!(e.attr.unwrap().size, 16),
    other => panic!("expected Entry, got {other:?}"),
  }
  assert_eq!(std::fs::read(container_ws.path().join("written.txt")).unwrap(), b"via container fs");

  match rpc(
    &tx,
    &mut rx,
    7,
    session_request::Body::Unlink(FsUnlink {
      parent: ROOT_INO,
      name: "written.txt".into(),
    }),
  )
  .await
  {
    session_response::Body::Ok(_) => {}
    other => panic!("expected Ok, got {other:?}"),
  }
  assert!(!container_ws.path().join("written.txt").exists());

  drop(tx);
  assert!(rx.message().await.unwrap().is_none());

  // Also verify operator-level pool resolution (`pool.named("build")`).
  let op_client =
    needy_api_client::Client::connect(&format!("unix:{}", control_socket.display())).await.unwrap().with_token("secret").unwrap();
  let (op_tx, mut op_rx) = op_client.fs_session().await.unwrap();
  match rpc(
    &op_tx,
    &mut op_rx,
    1,
    session_request::Body::Mount(FsMount {
      executor: "build".into(),
      source: container_path.into(),
      target: "/workspace".into(),
      writable: false,
      lock_mode: FsLockMode::Local as i32,
    }),
  )
  .await
  {
    session_response::Body::Mounted(m) => {
      assert!(!m.writable);
      assert_eq!(m.root.unwrap().ino, ROOT_INO);
    }
    other => panic!("expected Mounted via pool.named, got {other:?}"),
  }
  drop(op_tx);
  assert!(op_rx.message().await.unwrap().is_none());

  Runs::stop(&daemon, &run_info.id, Principal::Operator, StopMode::Now).await.unwrap();
  daemon.shutdown().await;
}
