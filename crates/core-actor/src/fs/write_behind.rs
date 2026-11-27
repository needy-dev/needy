//! Per-handle async write-behind pipeline.
//!
//! Pipelines FUSE `write` chunks onto concurrent in-flight `FsWrite` RPCs with
//! bounded dirty bytes and concurrency per handle, so WAN writes do not stall
//! per 128 KiB FUSE chunk. `flush` / `fsync` / `release` act as barriers that
//! await all in-flight writes and surface the first write error cleanly.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinHandle;

use super::client::{EIO, FsRpcClient};

/// Default maximum in-flight dirty bytes per open file handle: 4 MiB.
pub const DEFAULT_MAX_DIRTY_BYTES: usize = 4 * 1024 * 1024;
/// Default maximum concurrent write RPCs per open file handle.
pub const DEFAULT_MAX_INFLIGHT: usize = 16;

/// Backing write sink used by [`WriteBehind`].
#[async_trait::async_trait]
pub trait WriteSink: Send + Sync + 'static {
  /// Sends a single write chunk to the remote filesystem.
  async fn write_chunk(&self, ino: u64, fh: u64, offset: u64, data: Vec<u8>) -> Result<u32, i32>;
}

#[async_trait::async_trait]
impl WriteSink for FsRpcClient {
  async fn write_chunk(&self, ino: u64, fh: u64, offset: u64, data: Vec<u8>) -> Result<u32, i32> {
    self.write(ino, fh, offset, data).await.map(|w| w.written)
  }
}

/// Per-handle async write-behind pipeline.
pub struct WriteBehind<S: WriteSink = FsRpcClient> {
  sink: Arc<S>,
  max_dirty_bytes: usize,
  max_inflight: usize,
  handles: Mutex<HashMap<u64, Arc<HandleWriteState>>>,
}

struct HandleWriteState {
  dirty_bytes_sem: Arc<Semaphore>,
  inflight_sem: Arc<Semaphore>,
  written_any: AtomicBool,
  first_error: Arc<StdMutex<Option<i32>>>,
  tasks: Mutex<VecDeque<JoinHandle<()>>>,
}

impl HandleWriteState {
  fn new(max_dirty_bytes: usize, max_inflight: usize) -> Self {
    Self {
      dirty_bytes_sem: Arc::new(Semaphore::new(max_dirty_bytes.max(1))),
      inflight_sem: Arc::new(Semaphore::new(max_inflight.max(1))),
      written_any: AtomicBool::new(false),
      first_error: Arc::new(StdMutex::new(None)),
      tasks: Mutex::new(VecDeque::new()),
    }
  }

  fn record_error(&self, errno: i32) {
    if let Ok(mut guard) = self.first_error.lock()
      && guard.is_none()
    {
      *guard = Some(errno);
    }
  }

  fn peek_error(&self) -> Option<i32> {
    self.first_error.lock().ok().and_then(|g| *g)
  }

  fn take_error(&self) -> Option<i32> {
    self.first_error.lock().ok().and_then(|mut g| g.take())
  }

  async fn drain_tasks(&self) -> Result<(), i32> {
    let pending: Vec<JoinHandle<()>> = {
      let mut tasks = self.tasks.lock().await;
      tasks.drain(..).collect()
    };
    for handle in pending {
      if handle.await.is_err() {
        self.record_error(EIO);
      }
    }
    if let Some(errno) = self.take_error() {
      Err(errno)
    } else {
      Ok(())
    }
  }
}

impl<S: WriteSink> WriteBehind<S> {
  /// Creates a write-behind pipeline with default limits (4 MiB dirty bytes,
  /// 16 concurrent RPCs per handle).
  pub fn new(sink: Arc<S>) -> Self {
    Self::with_limits(sink, DEFAULT_MAX_DIRTY_BYTES, DEFAULT_MAX_INFLIGHT)
  }

  /// Creates a write-behind pipeline with custom per-handle limits.
  pub fn with_limits(sink: Arc<S>, max_dirty_bytes: usize, max_inflight: usize) -> Self {
    Self {
      sink,
      max_dirty_bytes: max_dirty_bytes.max(1),
      max_inflight: max_inflight.max(1),
      handles: Mutex::new(HashMap::new()),
    }
  }

  async fn get_or_create_handle(&self, fh: u64) -> Arc<HandleWriteState> {
    let mut map = self.handles.lock().await;
    Arc::clone(map.entry(fh).or_insert_with(|| Arc::new(HandleWriteState::new(self.max_dirty_bytes, self.max_inflight))))
  }

  /// Enqueues `data` at `offset` on `(ino, fh)` asynchronously.
  ///
  /// Applies backpressure when the handle's in-flight dirty bytes reach
  /// `max_dirty_bytes` or concurrent RPCs reach `max_inflight`, then returns
  /// immediately once the RPC is dispatched.
  pub async fn write(&self, ino: u64, fh: u64, offset: u64, data: Vec<u8>) -> Result<u32, i32> {
    let len = data.len();
    if len == 0 {
      return Ok(0);
    }

    let state = self.get_or_create_handle(fh).await;
    state.written_any.store(true, Ordering::Release);

    if let Some(errno) = state.peek_error() {
      return Err(errno);
    }

    let byte_permits = len.min(self.max_dirty_bytes) as u32;
    let dirty_permit = Arc::clone(&state.dirty_bytes_sem).acquire_many_owned(byte_permits).await.map_err(|_| EIO)?;
    let inflight_permit = Arc::clone(&state.inflight_sem).acquire_owned().await.map_err(|_| EIO)?;

    // Re-check after waiting for backpressure permits.
    if let Some(errno) = state.peek_error() {
      return Err(errno);
    }

    let sink = Arc::clone(&self.sink);
    let first_error = Arc::clone(&state.first_error);
    let expected = len as u32;

    let task = tokio::spawn(async move {
      let _dirty = dirty_permit;
      let _inflight = inflight_permit;
      match sink.write_chunk(ino, fh, offset, data).await {
        Ok(written) if written == expected => {}
        Ok(_) => {
          if let Ok(mut guard) = first_error.lock()
            && guard.is_none()
          {
            *guard = Some(EIO);
          }
        }
        Err(errno) => {
          if let Ok(mut guard) = first_error.lock()
            && guard.is_none()
          {
            *guard = Some(errno);
          }
        }
      }
    });

    let mut tasks = state.tasks.lock().await;
    while tasks.front().is_some_and(|t| t.is_finished()) {
      tasks.pop_front();
    }
    tasks.push_back(task);

    Ok(expected)
  }

  /// Waits for all in-flight writes on `fh` to complete and returns the first
  /// write error, if any.
  pub async fn flush(&self, fh: u64) -> Result<(), i32> {
    let state = {
      let map = self.handles.lock().await;
      map.get(&fh).cloned()
    };
    match state {
      Some(state) => state.drain_tasks().await,
      None => Ok(()),
    }
  }

  /// Waits for all in-flight writes on `fh`, removes its tracking state, and
  /// returns the first write error, if any.
  pub async fn release(&self, fh: u64) -> Result<(), i32> {
    let state = {
      let mut map = self.handles.lock().await;
      map.remove(&fh)
    };
    match state {
      Some(state) => state.drain_tasks().await,
      None => Ok(()),
    }
  }

  /// Returns `true` if any write has been issued on `fh` since it was opened.
  ///
  /// Used by FUSE `flush` to skip remote RPCs on zero-byte `dup` + `close`.
  pub async fn has_written(&self, fh: u64) -> bool {
    let map = self.handles.lock().await;
    map.get(&fh).is_some_and(|state| state.written_any.load(Ordering::Acquire))
  }
}
