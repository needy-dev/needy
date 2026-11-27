use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use needy_api_proto::v1::{
  FsAttr, FsAttrReply, FsData, FsError, FsForceUnmount, FsLockMode, FsMount, FsMounted, FsPong, SessionRequest, SessionResponse,
  session_request, session_response,
};
use tokio::sync::{Notify, mpsc};
use tokio_stream::wrappers::ReceiverStream;

use super::client::{EIO, FsRpcClient, SessionConnector, SessionResponseStream};
use super::neg_cache::NegativeCache;
use super::prefetch::{BACKWARD_SEEK_WINDOW, FORWARD_SKIP_TOLERANCE, INITIAL_REQUEST_SIZE, MAX_REQUEST_SIZE, Prefetcher, ReadSource};
use super::write_behind::{WriteBehind, WriteSink};

// ---------------------------------------------------------------------------
// NegativeCache tests
// ---------------------------------------------------------------------------

#[test]
fn fs_ttl_env_parses_positive_seconds_only() {
  assert_eq!(super::parse_ttl_secs(None), None);
  assert_eq!(super::parse_ttl_secs(Some("")), None);
  assert_eq!(super::parse_ttl_secs(Some("0")), None);
  assert_eq!(super::parse_ttl_secs(Some("abc")), None);
  assert_eq!(super::parse_ttl_secs(Some("15")), Some(Duration::from_secs(15)));
  assert_eq!(super::parse_ttl_secs(Some(" 30 ")), Some(Duration::from_secs(30)));
}

#[test]
fn neg_cache_expiry_and_invalidation() {
  let cache = NegativeCache::new(16, Duration::from_millis(40));
  cache.insert(1, "missing.txt");
  cache.insert(1, "other.txt");
  cache.insert(2, "sibling.txt");

  assert!(cache.contains(1, "missing.txt"));
  assert!(cache.contains(1, "other.txt"));
  assert!(cache.contains(2, "sibling.txt"));

  cache.invalidate(1, "missing.txt");
  assert!(!cache.contains(1, "missing.txt"));
  assert!(cache.contains(1, "other.txt"));

  cache.invalidate_parent(1);
  assert!(!cache.contains(1, "other.txt"));
  assert!(cache.contains(2, "sibling.txt"));

  std::thread::sleep(Duration::from_millis(60));
  assert!(!cache.contains(2, "sibling.txt"));
}

#[test]
fn neg_cache_fifo_eviction() {
  let cache = NegativeCache::new(3, Duration::from_secs(60));
  cache.insert(1, "a");
  cache.insert(1, "b");
  cache.insert(1, "c");
  assert_eq!(cache.len(), 3);

  cache.insert(1, "d");
  assert_eq!(cache.len(), 3);
  assert!(!cache.contains(1, "a"));
  assert!(cache.contains(1, "b"));
  assert!(cache.contains(1, "c"));
  assert!(cache.contains(1, "d"));

  cache.clear();
  assert!(cache.is_empty());
}

// ROOT CAUSE:
//
// On case-insensitive executors (Windows NTFS, macOS APFS) where
// `FsMounted.case_insensitive == true`, a negative lookup for `"FOO.TXT"`
// followed by creating `"foo.txt"` invalidated only the exact key `"foo.txt"`.
//
// Before the fix, `"FOO.TXT"` remained in `NegativeCache` and an immediate
// `open("FOO.TXT")` failed with stale `ENOENT` until the 1s TTL expired.
// The fix folds keys to lowercase when `case_insensitive` is true while
// keeping case-sensitive caches distinct.
#[test]
fn neg_cache_case_insensitivity_evicts_across_casing() {
  let insensitive = NegativeCache::with_case_sensitivity(16, Duration::from_secs(60), true);
  insensitive.insert(1, "FOO.TXT");
  assert!(insensitive.contains(1, "FOO.TXT"));
  assert!(insensitive.contains(1, "foo.txt"));
  assert!(insensitive.contains(1, "Foo.Txt"));

  insensitive.invalidate(1, "foo.txt");
  assert!(!insensitive.contains(1, "FOO.TXT"));
  assert!(!insensitive.contains(1, "foo.txt"));
  assert!(insensitive.is_empty());

  let sensitive = NegativeCache::with_case_sensitivity(16, Duration::from_secs(60), false);
  sensitive.insert(1, "FOO.TXT");
  assert!(sensitive.contains(1, "FOO.TXT"));
  assert!(!sensitive.contains(1, "foo.txt"));

  sensitive.invalidate(1, "foo.txt");
  assert!(sensitive.contains(1, "FOO.TXT"));
  sensitive.invalidate(1, "FOO.TXT");
  assert!(!sensitive.contains(1, "FOO.TXT"));
}

// ---------------------------------------------------------------------------
// Prefetcher tests
// ---------------------------------------------------------------------------

struct MockReadSource {
  file_size: u64,
  requests: Mutex<Vec<(u64, u32)>>,
}

impl MockReadSource {
  fn new(file_size: u64) -> Arc<Self> {
    Arc::new(Self {
      file_size,
      requests: Mutex::new(Vec::new()),
    })
  }

  fn recorded(&self) -> Vec<(u64, u32)> {
    self.requests.lock().unwrap().clone()
  }

  fn byte_at(offset: u64) -> u8 {
    ((offset / 4096) ^ (offset & 0xff)) as u8
  }
}

#[async_trait::async_trait]
impl ReadSource for MockReadSource {
  async fn read_range(&self, _ino: u64, _fh: u64, offset: u64, size: u32) -> Result<FsData, i32> {
    self.requests.lock().unwrap().push((offset, size));
    if offset >= self.file_size {
      return Ok(FsData {
        data: Vec::new(),
        eof: true,
      });
    }
    let avail = (self.file_size - offset).min(size as u64) as usize;
    let mut data = Vec::with_capacity(avail);
    for i in 0..avail {
      data.push(Self::byte_at(offset + i as u64));
    }
    let eof = offset + avail as u64 >= self.file_size;
    Ok(FsData { data, eof })
  }
}

#[tokio::test]
async fn prefetcher_two_stage_scaling_and_backward_seek() {
  let file_size = 64 * 1024 * 1024;
  let source = MockReadSource::new(file_size);
  let prefetcher = Prefetcher::new(Arc::clone(&source));

  // Read sequentially in 128 KiB chunks across multiple prefetch windows.
  let chunk_size = 128 * 1024u32;
  let mut offset = 0u64;
  let target = (INITIAL_REQUEST_SIZE * 4) as u64;
  while offset < target {
    let data = prefetcher.read(10, 1, offset, chunk_size).await.unwrap();
    assert_eq!(data.len(), chunk_size as usize);
    assert_eq!(data[0], MockReadSource::byte_at(offset));
    assert_eq!(data[data.len() - 1], MockReadSource::byte_at(offset + chunk_size as u64 - 1));
    offset += chunk_size as u64;
  }

  let reqs = source.recorded();
  assert!(reqs.len() >= 3, "expected scaled prefetch requests, got {reqs:?}");
  assert_eq!(reqs[0], (0, INITIAL_REQUEST_SIZE as u32));
  assert_eq!(reqs[1].1, (INITIAL_REQUEST_SIZE * 2).min(MAX_REQUEST_SIZE) as u32);
  assert_eq!(reqs[2].1, (INITIAL_REQUEST_SIZE * 4).min(MAX_REQUEST_SIZE) as u32);

  // Backward seek within 1 MiB should hit the backward SeekWindow with 0 RPCs.
  let reqs_before = source.recorded().len();
  let back_offset = offset - (BACKWARD_SEEK_WINDOW as u64 / 2);
  let back = prefetcher.read(10, 1, back_offset, 4096).await.unwrap();
  assert_eq!(back.len(), 4096);
  assert_eq!(back[0], MockReadSource::byte_at(back_offset));
  assert_eq!(source.recorded().len(), reqs_before, "backward seek should not issue RPCs");
}

#[tokio::test]
async fn prefetcher_tolerates_out_of_order_readahead_and_resets_on_random_seek() {
  let file_size = 64 * 1024 * 1024;
  let source = MockReadSource::new(file_size);
  let prefetcher = Prefetcher::new(Arc::clone(&source));

  // Initial read starts the 2-stage pipeline (stage 1: 1.125 MiB, stage 2: 2.25 MiB).
  let first = prefetcher.read(10, 2, 0, 128 * 1024).await.unwrap();
  assert_eq!(first.len(), 128 * 1024);

  // Out-of-order kernel readahead: read at 256 KiB before 128 KiB.
  let reqs_before = source.recorded().len();
  let ooo = prefetcher.read(10, 2, 256 * 1024, 128 * 1024).await.unwrap();
  assert_eq!(ooo.len(), 128 * 1024);
  assert_eq!(ooo[0], MockReadSource::byte_at(256 * 1024));
  let earlier = prefetcher.read(10, 2, 128 * 1024, 128 * 1024).await.unwrap();
  assert_eq!(earlier[0], MockReadSource::byte_at(128 * 1024));
  assert_eq!(source.recorded().len(), reqs_before, "out-of-order readahead inside window must not cancel prefetch stream");

  // Random seek far past FORWARD_SKIP_TOLERANCE resets window back to INITIAL_REQUEST_SIZE.
  let far_offset = (INITIAL_REQUEST_SIZE as u64) * 4 + FORWARD_SKIP_TOLERANCE + 1024 * 1024;
  let jumped = prefetcher.read(10, 2, far_offset, 4096).await.unwrap();
  assert_eq!(jumped.len(), 4096);
  assert_eq!(jumped[0], MockReadSource::byte_at(far_offset));

  let reqs = source.recorded();
  let reset_req = reqs.iter().find(|&&(off, _)| off == far_offset).copied();
  assert_eq!(reset_req, Some((far_offset, INITIAL_REQUEST_SIZE as u32)), "random seek should reset request size to INITIAL_REQUEST_SIZE");
}

// ---------------------------------------------------------------------------
// WriteBehind tests
// ---------------------------------------------------------------------------

struct MockWriteSink {
  inflight: AtomicUsize,
  max_inflight: AtomicUsize,
  writes: Mutex<HashMap<u64, Vec<u8>>>,
  gate: Option<Arc<Notify>>,
  fail_offset: Option<(u64, i32)>,
}

impl MockWriteSink {
  fn new(gate: Option<Arc<Notify>>, fail_offset: Option<(u64, i32)>) -> Arc<Self> {
    Arc::new(Self {
      inflight: AtomicUsize::new(0),
      max_inflight: AtomicUsize::new(0),
      writes: Mutex::new(HashMap::new()),
      gate,
      fail_offset,
    })
  }
}

#[async_trait::async_trait]
impl WriteSink for MockWriteSink {
  async fn write_chunk(&self, _ino: u64, _fh: u64, offset: u64, data: Vec<u8>) -> Result<u32, i32> {
    let cur = self.inflight.fetch_add(1, Ordering::SeqCst) + 1;
    self.max_inflight.fetch_max(cur, Ordering::SeqCst);

    if let Some(gate) = &self.gate {
      gate.notified().await;
    }

    self.inflight.fetch_sub(1, Ordering::SeqCst);

    if let Some((bad_offset, errno)) = self.fail_offset
      && offset == bad_offset
    {
      return Err(errno);
    }

    let len = data.len() as u32;
    self.writes.lock().unwrap().insert(offset, data);
    Ok(len)
  }
}

#[tokio::test]
async fn write_behind_pipelines_concurrently_and_orders_flush_barrier() {
  let gate = Arc::new(Notify::new());
  let sink = MockWriteSink::new(Some(Arc::clone(&gate)), None);
  let wb = WriteBehind::new(Arc::clone(&sink));

  assert!(!wb.has_written(7).await);

  // Enqueue 4 chunks; none have finished yet because `gate` is held.
  for i in 0..4u64 {
    let written = wb.write(1, 7, i * 1024, vec![i as u8; 1024]).await.unwrap();
    assert_eq!(written, 1024);
  }
  assert!(wb.has_written(7).await);

  // Give spawned tasks a moment to enter `write_chunk` concurrently.
  tokio::time::sleep(Duration::from_millis(20)).await;
  assert!(sink.max_inflight.load(Ordering::SeqCst) >= 2, "expected concurrent pipelined writes");

  // Start flush barrier while writes are still in flight, then release gate.
  let flush_task = {
    let wb_flush = async { wb.flush(7).await };
    let release_gate = async {
      tokio::time::sleep(Duration::from_millis(15)).await;
      gate.notify_waiters();
    };
    let (res, ()) = tokio::join!(wb_flush, release_gate);
    res
  };
  flush_task.unwrap();

  let recorded = sink.writes.lock().unwrap();
  assert_eq!(recorded.len(), 4);
  for i in 0..4u64 {
    assert_eq!(recorded.get(&(i * 1024)).unwrap(), &vec![i as u8; 1024]);
  }
}

#[tokio::test]
async fn write_behind_propagates_first_error_on_flush() {
  const ENOSPC: i32 = 28;
  let sink = MockWriteSink::new(None, Some((1024, ENOSPC)));
  let wb = WriteBehind::new(Arc::clone(&sink));

  wb.write(1, 9, 0, vec![1; 1024]).await.unwrap();
  wb.write(1, 9, 1024, vec![2; 1024]).await.unwrap();

  let err = wb.flush(9).await.unwrap_err();
  assert_eq!(err, ENOSPC);
}

// ---------------------------------------------------------------------------
// FsRpcClient tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rpc_client_multiplexes_out_of_order_responses() {
  let (req_tx, mut req_rx) = mpsc::channel::<SessionRequest>(16);
  let (resp_tx, resp_rx) = mpsc::channel::<Result<SessionResponse, tonic::Status>>(16);

  let client = FsRpcClient::from_stream(req_tx, ReceiverStream::new(resp_rx));

  // Mock server that collects two requests and replies in reverse order.
  tokio::spawn(async move {
    let r1 = req_rx.recv().await.unwrap();
    let r2 = req_rx.recv().await.unwrap();

    // Reply to r2 first, then r1.
    for r in [r2, r1] {
      let body = match r.body.unwrap() {
        session_request::Body::Ping(p) => session_response::Body::Pong(FsPong {
          timestamp_ms: p.timestamp_ms,
        }),
        session_request::Body::Getattr(g) => session_response::Body::Attr(FsAttrReply {
          attr: Some(FsAttr {
            ino: g.ino,
            size: g.ino * 10,
            ..Default::default()
          }),
        }),
        _ => session_response::Body::Error(FsError {
          errno: EIO,
          message: "unexpected".into(),
        }),
      };
      resp_tx
        .send(Ok(SessionResponse {
          id: r.id,
          body: Some(body),
        }))
        .await
        .unwrap();
    }
  });

  let (pong, attr) = tokio::join!(client.ping(12345), client.getattr(42, None));
  assert_eq!(pong.unwrap().timestamp_ms, 12345);
  assert_eq!(attr.unwrap().size, 420);
}

#[tokio::test]
async fn rpc_client_fails_fast_with_eio_on_disconnect() {
  let (req_tx, mut req_rx) = mpsc::channel::<SessionRequest>(8);
  let (resp_tx, resp_rx) = mpsc::channel::<Result<SessionResponse, tonic::Status>>(8);

  let client = FsRpcClient::from_stream(req_tx, ReceiverStream::new(resp_rx));
  let cancel = client.cancelled();

  // Receive the request and immediately drop the response stream (disconnect).
  tokio::spawn(async move {
    let _req = req_rx.recv().await.unwrap();
    drop(resp_tx);
  });

  let err = client.ping(1).await.unwrap_err();
  assert_eq!(err, EIO);
  assert!(cancel.is_cancelled());

  // Subsequent calls after disconnect also fail immediately with EIO.
  assert_eq!(client.ping(2).await.unwrap_err(), EIO);
}

#[tokio::test]
async fn rpc_client_cancels_on_force_unmount() {
  let (req_tx, mut req_rx) = mpsc::channel::<SessionRequest>(8);
  let (resp_tx, resp_rx) = mpsc::channel::<Result<SessionResponse, tonic::Status>>(8);

  let client = FsRpcClient::from_stream(req_tx, ReceiverStream::new(resp_rx));
  let cancel = client.cancelled();

  tokio::spawn(async move {
    let _req = req_rx.recv().await.unwrap();
    resp_tx
      .send(Ok(SessionResponse {
        id: 0,
        body: Some(session_response::Body::ForceUnmount(FsForceUnmount {
          reason: "run stopped".into(),
        })),
      }))
      .await
      .unwrap();
  });

  let err = client.getattr(1, None).await.unwrap_err();
  assert_eq!(err, EIO);
  assert!(cancel.is_cancelled());
  assert_eq!(client.force_unmount_reason().as_deref(), Some("run stopped"));
}

fn sample_mount_req() -> FsMount {
  FsMount {
    executor: "runner-x".into(),
    source: "/workspace".into(),
    target: "/home/actor/workspace".into(),
    writable: true,
    lock_mode: FsLockMode::Local.into(),
  }
}

// ROOT CAUSE:
//
// If the runner's transport dropped transiently (e.g. idle-proxy cut),
// `FsRpcClient` cancelled its mount token immediately on stream EOF instead of
// reconnecting within the runner grace window.
//
// Before the fix, any unexpected stream close unmounted FUSE3 immediately.
// The fix fails in-flight RPCs with EIO, reconnects via the connector, replays
// `FsMount`, and resumes serving without cancelling the mount.
#[tokio::test]
async fn rpc_client_reconnects_after_transient_disconnect_and_replays_mount() {
  let (req_tx1, mut req_rx1) = mpsc::channel::<SessionRequest>(8);
  let (resp_tx1, resp_rx1) = mpsc::channel::<Result<SessionResponse, tonic::Status>>(8);

  let mount_req = sample_mount_req();
  let expected_mount = mount_req.clone();
  let attempts = Arc::new(AtomicUsize::new(0));
  let replayed_mount = Arc::new(Mutex::new(None::<FsMount>));

  let attempts_for_conn = Arc::clone(&attempts);
  let replayed_for_conn = Arc::clone(&replayed_mount);
  let connector: SessionConnector = Arc::new(move || {
    let attempt = attempts_for_conn.fetch_add(1, Ordering::SeqCst);
    let replayed = Arc::clone(&replayed_for_conn);
    Box::pin(async move {
      let (tx, mut rx) = mpsc::channel::<SessionRequest>(8);
      let (resp_tx, resp_rx) = mpsc::channel::<Result<SessionResponse, tonic::Status>>(8);

      tokio::spawn(async move {
        let first = rx.recv().await.unwrap();
        if attempt == 0 {
          // Simulate daemon returning EIO to FsMount while the runner is still
          // reconnecting.
          let _ = resp_tx
            .send(Ok(SessionResponse {
              id: first.id,
              body: Some(session_response::Body::Error(FsError {
                errno: EIO,
                message: "runner not connected".into(),
              })),
            }))
            .await;
          return;
        }

        match first.body {
          Some(session_request::Body::Mount(m)) => {
            *replayed.lock().unwrap() = Some(m);
            resp_tx
              .send(Ok(SessionResponse {
                id: first.id,
                body: Some(session_response::Body::Mounted(FsMounted {
                  writable: true,
                  lock_mode: FsLockMode::Local.into(),
                  ..Default::default()
                })),
              }))
              .await
              .unwrap();
          }
          other => panic!("expected replayed FsMount, got {other:?}"),
        }

        while let Some(req) = rx.recv().await {
          let body = match req.body.unwrap() {
            session_request::Body::Ping(p) => session_response::Body::Pong(FsPong {
              timestamp_ms: p.timestamp_ms,
            }),
            session_request::Body::Getattr(g) => session_response::Body::Attr(FsAttrReply {
              attr: Some(FsAttr {
                ino: g.ino,
                size: 77,
                ..Default::default()
              }),
            }),
            _ => session_response::Body::Error(FsError {
              errno: EIO,
              message: "unexpected".into(),
            }),
          };
          if resp_tx
            .send(Ok(SessionResponse {
              id: req.id,
              body: Some(body),
            }))
            .await
            .is_err()
          {
            break;
          }
        }
      });

      let stream: SessionResponseStream = Box::pin(ReceiverStream::new(resp_rx));
      Ok((tx, stream))
    })
  });

  let client = FsRpcClient::with_connector(req_tx1, ReceiverStream::new(resp_rx1), connector, mount_req, Duration::from_secs(5));
  let cancel = client.cancelled();

  // Receive the initial request and drop the first stream without replying.
  tokio::spawn(async move {
    let _req = req_rx1.recv().await.unwrap();
    drop(resp_tx1);
  });

  // In-flight call fails with EIO, but the mount is NOT cancelled.
  let err = client.ping(1).await.unwrap_err();
  assert_eq!(err, EIO);
  assert!(!cancel.is_cancelled());

  // Subsequent calls wait through reconnect and succeed on the new session.
  let pong = client.ping(999).await.unwrap();
  assert_eq!(pong.timestamp_ms, 999);
  let attr = client.getattr(1, None).await.unwrap();
  assert_eq!(attr.ino, 1);
  assert_eq!(attr.size, 77);

  assert!(!cancel.is_cancelled());
  assert_eq!(client.session_epoch(), 1);
  assert_eq!(attempts.load(Ordering::SeqCst), 2);
  assert_eq!(replayed_mount.lock().unwrap().as_ref(), Some(&expected_mount));
}

#[tokio::test]
async fn rpc_client_with_connector_cancels_immediately_on_force_unmount() {
  let (req_tx, mut req_rx) = mpsc::channel::<SessionRequest>(8);
  let (resp_tx, resp_rx) = mpsc::channel::<Result<SessionResponse, tonic::Status>>(8);

  let attempts = Arc::new(AtomicUsize::new(0));
  let attempts_for_conn = Arc::clone(&attempts);
  let connector: SessionConnector = Arc::new(move || {
    attempts_for_conn.fetch_add(1, Ordering::SeqCst);
    Box::pin(async move { Err("connector must not be called on force unmount".into()) })
  });

  let client = FsRpcClient::with_connector(req_tx, ReceiverStream::new(resp_rx), connector, sample_mount_req(), Duration::from_secs(5));
  let cancel = client.cancelled();

  tokio::spawn(async move {
    let _req = req_rx.recv().await.unwrap();
    resp_tx
      .send(Ok(SessionResponse {
        id: 0,
        body: Some(session_response::Body::ForceUnmount(FsForceUnmount {
          reason: "run stopped".into(),
        })),
      }))
      .await
      .unwrap();
  });

  let err = client.ping(1).await.unwrap_err();
  assert_eq!(err, EIO);
  assert!(cancel.is_cancelled());
  assert_eq!(client.force_unmount_reason().as_deref(), Some("run stopped"));
  assert_eq!(attempts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn rpc_client_cancels_when_reconnect_window_exhausted() {
  let (req_tx, mut req_rx) = mpsc::channel::<SessionRequest>(8);
  let (resp_tx, resp_rx) = mpsc::channel::<Result<SessionResponse, tonic::Status>>(8);

  let attempts = Arc::new(AtomicUsize::new(0));
  let attempts_for_conn = Arc::clone(&attempts);
  let connector: SessionConnector = Arc::new(move || {
    attempts_for_conn.fetch_add(1, Ordering::SeqCst);
    Box::pin(async move { Err("runner unavailable".into()) })
  });

  let client = FsRpcClient::with_connector(req_tx, ReceiverStream::new(resp_rx), connector, sample_mount_req(), Duration::from_millis(120));
  let cancel = client.cancelled();

  tokio::spawn(async move {
    let _req = req_rx.recv().await.unwrap();
    drop(resp_tx);
  });

  let err = client.ping(1).await.unwrap_err();
  assert_eq!(err, EIO);

  // Wait for the reconnect window to expire and cancel the mount.
  tokio::time::timeout(Duration::from_secs(2), cancel.cancelled()).await.expect("expected cancellation after reconnect window");
  assert!(client.is_cancelled());
  assert!(attempts.load(Ordering::SeqCst) >= 1);
  assert_eq!(client.ping(2).await.unwrap_err(), EIO);
}

// ---------------------------------------------------------------------------
// Workspace mount backend selection & UNIMPLEMENTED detection tests
// ---------------------------------------------------------------------------

#[test]
fn fs_backend_parse_handles_defaults_and_overrides() {
  use super::FsBackend;

  assert_eq!(FsBackend::parse(None), FsBackend::Auto);
  assert_eq!(FsBackend::parse(Some("")), FsBackend::Auto);
  assert_eq!(FsBackend::parse(Some("auto")), FsBackend::Auto);
  assert_eq!(FsBackend::parse(Some("unknown")), FsBackend::Auto);
  assert_eq!(FsBackend::parse(Some("sshfs")), FsBackend::Sshfs);
  assert_eq!(FsBackend::parse(Some(" SSHFS ")), FsBackend::Sshfs);
  assert_eq!(FsBackend::parse(Some("fuse3")), FsBackend::Fuse3);
  assert_eq!(FsBackend::parse(Some("FUSE3")), FsBackend::Fuse3);
}

#[cfg(unix)]
#[test]
fn is_unimplemented_detects_direct_and_wrapped_grpc_statuses() {
  use super::is_unimplemented;

  let direct_client_err: anyhow::Error = needy_api_client::Error::Status(tonic::Status::unimplemented("no FsService")).into();
  assert!(is_unimplemented(&direct_client_err));

  let wrapped_client_err = direct_client_err.context("mount /workspace");
  assert!(is_unimplemented(&wrapped_client_err));

  let direct_status: anyhow::Error = tonic::Status::unimplemented("missing method").into();
  assert!(is_unimplemented(&direct_status));

  let permission_denied: anyhow::Error = needy_api_client::Error::Status(tonic::Status::permission_denied("denied")).into();
  assert!(!is_unimplemented(&permission_denied));

  let other_err = anyhow::anyhow!("unrelated io failure");
  assert!(!is_unimplemented(&other_err));
}
