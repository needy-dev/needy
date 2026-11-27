//! Adaptive sequential read prefetcher for high-latency WAN links.
//!
//! Adapted from the `awslabs/mountpoint-s3` prefetcher design:
//! - Initial fetch size: `1 MiB + 128 KiB` (`1_179_648` bytes), so a single
//!   sequential burst covers multiple 128 KiB FUSE reads.
//! - 2-stage fetch pipeline with 2x window scaling up to `16 MiB` as sequential
//!   reads consume buffered chunks.
//! - `1 MiB` backward `SeekWindow` ring so small backward seeks hit RAM at 0 RTT.
//! - `4 MiB` forward-skip tolerance so out-of-order kernel readahead does not
//!   cancel active prefetch streams.
//! - Invalidated whenever writes or truncates touch the same inode.
// Reference: https://github.com/awslabs/mountpoint-s3/blob/main/mountpoint-s3-fs/src/prefetch/part_stream.rs
// (design reference read 2026-11-27: adaptive sequential window doubling,
// backward seek window, and forward-skip tolerance).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use bytes::{Bytes, BytesMut};
use needy_api_proto::v1::FsData;
use tokio::sync::{Mutex, oneshot};

use super::client::{EIO, FsRpcClient};

/// Initial prefetch request size: 1 MiB + 128 KiB.
pub const INITIAL_REQUEST_SIZE: usize = 1024 * 1024 + 128 * 1024;
/// Maximum adaptive prefetch window size: 16 MiB.
pub const MAX_REQUEST_SIZE: usize = 16 * 1024 * 1024;
/// Maximum retained backward seek window per open handle: 1 MiB.
pub const BACKWARD_SEEK_WINDOW: usize = 1024 * 1024;
/// Forward skip distance tolerated without resetting the prefetch stream: 4 MiB.
pub const FORWARD_SKIP_TOLERANCE: u64 = 4 * 1024 * 1024;
/// Number of pipelined prefetch stages kept in flight.
const MAX_INFLIGHT_PARTS: usize = 2;

/// Backing byte-range reader used by [`Prefetcher`].
#[async_trait::async_trait]
pub trait ReadSource: Send + Sync + 'static {
  /// Reads up to `size` bytes starting at `offset` from `(ino, fh)`.
  async fn read_range(&self, ino: u64, fh: u64, offset: u64, size: u32) -> Result<FsData, i32>;
}

#[async_trait::async_trait]
impl ReadSource for FsRpcClient {
  async fn read_range(&self, ino: u64, fh: u64, offset: u64, size: u32) -> Result<FsData, i32> {
    self.read(ino, fh, offset, size).await
  }
}

/// Per-handle adaptive read prefetcher.
pub struct Prefetcher<S: ReadSource = FsRpcClient> {
  source: Arc<S>,
  handles: Mutex<HashMap<u64, Arc<Mutex<HandleState>>>>,
}

#[derive(Clone)]
struct CompletedChunk {
  offset: u64,
  data: Bytes,
  eof: bool,
}

impl CompletedChunk {
  fn end(&self) -> u64 {
    self.offset.saturating_add(self.data.len() as u64)
  }

  fn slice_from(&self, offset: u64, max_len: usize) -> Option<Bytes> {
    if offset < self.offset || offset >= self.end() {
      return None;
    }
    let start = (offset - self.offset) as usize;
    let take = (self.data.len() - start).min(max_len);
    Some(self.data.slice(start..start + take))
  }
}

enum PartState {
  Pending(oneshot::Receiver<Result<CompletedChunk, i32>>),
  Ready(CompletedChunk),
}

struct InflightPart {
  offset: u64,
  requested_size: usize,
  state: PartState,
}

struct HandleState {
  ino: u64,
  fh: u64,
  next_request_offset: u64,
  next_request_size: usize,
  eof_offset: Option<u64>,
  backward_window: VecDeque<CompletedChunk>,
  backward_bytes: usize,
  queue: VecDeque<InflightPart>,
}

impl<S: ReadSource> Prefetcher<S> {
  /// Creates a new prefetcher backed by `source`.
  pub fn new(source: Arc<S>) -> Self {
    Self {
      source,
      handles: Mutex::new(HashMap::new()),
    }
  }

  /// Reads up to `size` bytes at `offset` from `(ino, fh)`, serving from the
  /// backward seek window or adaptive prefetch pipeline when possible.
  pub async fn read(&self, ino: u64, fh: u64, offset: u64, size: u32) -> Result<Bytes, i32> {
    if size == 0 {
      return Ok(Bytes::new());
    }

    let handle = {
      let mut map = self.handles.lock().await;
      Arc::clone(map.entry(fh).or_insert_with(|| Arc::new(Mutex::new(HandleState::new(ino, fh)))))
    };

    let mut state = handle.lock().await;
    state.read_from(&self.source, offset, size as usize).await
  }

  /// Drops all cached and in-flight prefetch state for every handle pointing to `ino`.
  pub async fn invalidate_inode(&self, ino: u64) {
    let mut map = self.handles.lock().await;
    let mut to_remove = Vec::new();
    for (&fh, handle) in map.iter() {
      let mut state = handle.lock().await;
      if state.ino == ino {
        state.reset(0);
        state.backward_window.clear();
        state.backward_bytes = 0;
        to_remove.push(fh);
      }
    }
    for fh in to_remove {
      map.remove(&fh);
    }
  }

  /// Releases prefetch state for a closed file handle.
  pub async fn release_handle(&self, fh: u64) {
    let mut map = self.handles.lock().await;
    map.remove(&fh);
  }

  /// Clears all handle state.
  pub async fn clear(&self) {
    let mut map = self.handles.lock().await;
    map.clear();
  }
}

impl HandleState {
  fn new(ino: u64, fh: u64) -> Self {
    Self {
      ino,
      fh,
      next_request_offset: 0,
      next_request_size: INITIAL_REQUEST_SIZE,
      eof_offset: None,
      backward_window: VecDeque::new(),
      backward_bytes: 0,
      queue: VecDeque::new(),
    }
  }

  fn reset(&mut self, start_offset: u64) {
    self.queue.clear();
    self.eof_offset = None;
    self.next_request_offset = start_offset;
    self.next_request_size = INITIAL_REQUEST_SIZE;
  }

  fn push_backward(&mut self, chunk: CompletedChunk) {
    if chunk.data.is_empty() {
      return;
    }
    // Avoid duplicating an identical chunk already at the back.
    if let Some(last) = self.backward_window.back()
      && last.offset == chunk.offset
      && last.data.len() == chunk.data.len()
    {
      return;
    }
    let chunk = if chunk.data.len() > BACKWARD_SEEK_WINDOW {
      let trim = chunk.data.len() - BACKWARD_SEEK_WINDOW;
      CompletedChunk {
        offset: chunk.offset + trim as u64,
        data: chunk.data.slice(trim..),
        eof: chunk.eof,
      }
    } else {
      chunk
    };
    self.backward_bytes += chunk.data.len();
    self.backward_window.push_back(chunk);
    while self.backward_bytes > BACKWARD_SEEK_WINDOW {
      let Some(evicted) = self.backward_window.pop_front() else {
        break;
      };
      self.backward_bytes = self.backward_bytes.saturating_sub(evicted.data.len());
    }
  }

  fn try_read_backward(&self, offset: u64, size: usize) -> Option<Bytes> {
    // Check if the requested start offset is present in the backward window.
    let mut cursor = offset;
    let mut remaining = size;
    let mut first: Option<Bytes> = None;
    let mut combined: Option<BytesMut> = None;

    for chunk in &self.backward_window {
      if let Some(slice) = chunk.slice_from(cursor, remaining) {
        let len = slice.len();
        cursor += len as u64;
        remaining -= len;
        if let Some(buf) = &mut combined {
          buf.extend_from_slice(&slice);
        } else if let Some(prev) = first.take() {
          let mut buf = BytesMut::with_capacity(prev.len() + len);
          buf.extend_from_slice(&prev);
          buf.extend_from_slice(&slice);
          combined = Some(buf);
        } else {
          first = Some(slice);
        }
        if remaining == 0 {
          break;
        }
      }
    }

    if let Some(buf) = combined {
      Some(buf.freeze())
    } else {
      first
    }
  }

  fn spawn_part<S: ReadSource>(&mut self, source: &Arc<S>, offset: u64, size: usize) {
    let size = size.min(u32::MAX as usize);
    let (tx, rx) = oneshot::channel();
    let src = Arc::clone(source);
    let ino = self.ino;
    let fh = self.fh;
    tokio::spawn(async move {
      let res = src.read_range(ino, fh, offset, size as u32).await.map(|d| CompletedChunk {
        offset,
        data: Bytes::from(d.data),
        eof: d.eof,
      });
      // Receiver may be dropped if the stream was reset on random seek.
      let _ = tx.send(res);
    });
    self.queue.push_back(InflightPart {
      offset,
      requested_size: size,
      state: PartState::Pending(rx),
    });
  }

  fn fill_pipeline<S: ReadSource>(&mut self, source: &Arc<S>, min_size: usize) {
    while self.queue.len() < MAX_INFLIGHT_PARTS && self.eof_offset.is_none() {
      let req_size = self.next_request_size.max(min_size).min(MAX_REQUEST_SIZE);
      let req_offset = self.next_request_offset;
      self.spawn_part(source, req_offset, req_size);
      self.next_request_offset = req_offset.saturating_add(req_size as u64);
      self.next_request_size = (self.next_request_size.saturating_mul(2)).min(MAX_REQUEST_SIZE);
    }
  }

  async fn read_from<S: ReadSource>(&mut self, source: &Arc<S>, offset: u64, size: usize) -> Result<Bytes, i32> {
    // 1. Check the backward seek window first (0 RTT, preserves active stream).
    if let Some(hit) = self.try_read_backward(offset, size)
      && (hit.len() == size || self.eof_offset.is_some_and(|eof| offset + hit.len() as u64 >= eof))
    {
      return Ok(hit);
    }

    // 2. Decide whether this read continues the active prefetch stream or is a random seek.
    if let Some(front) = self.queue.front() {
      let front_offset = front.offset;
      let pipeline_end = self.next_request_offset;
      if offset < front_offset {
        // Backward seek outside the backward window -> reset stream.
        self.reset(offset);
      } else if offset >= pipeline_end.saturating_add(FORWARD_SKIP_TOLERANCE) {
        // Large forward random seek -> reset stream.
        self.reset(offset);
      }
      // Otherwise: either inside `[front_offset, pipeline_end)` (sequential or
      // out-of-order readahead) or within `FORWARD_SKIP_TOLERANCE` ahead of
      // `pipeline_end` — preserve the active stream and window size!
    } else {
      if let Some(eof) = self.eof_offset
        && offset >= eof
      {
        return Ok(Bytes::new());
      }
      // Cold start or post-EOF seek.
      if self.next_request_offset != offset {
        self.reset(offset);
      }
    }

    // 3. Assemble up to `size` bytes starting at `offset`.
    let mut cursor = offset;
    let mut remaining = size;
    let mut first: Option<Bytes> = None;
    let mut combined: Option<BytesMut> = None;

    while remaining > 0 {
      if let Some(eof) = self.eof_offset
        && cursor >= eof
      {
        break;
      }

      self.fill_pipeline(source, remaining);

      let Some(front) = self.queue.front_mut() else {
        break;
      };

      // Resolve the front part if still in flight.
      let chunk = match &mut front.state {
        PartState::Ready(chunk) => chunk.clone(),
        PartState::Pending(_) => {
          let PartState::Pending(rx) = std::mem::replace(
            &mut front.state,
            PartState::Ready(CompletedChunk {
              offset: front.offset,
              data: Bytes::new(),
              eof: false,
            }),
          ) else {
            unreachable!();
          };
          match rx.await {
            Ok(Ok(chunk)) => {
              let short_read = chunk.data.len() < front.requested_size;
              let eof_hit = chunk.eof || short_read;
              let eof_pos = chunk.end();
              front.state = PartState::Ready(chunk.clone());
              if eof_hit {
                self.eof_offset = Some(eof_pos);
                // Drop any speculative parts queued past EOF.
                while self.queue.len() > 1 && self.queue.back().is_some_and(|p| p.offset >= eof_pos) {
                  self.queue.pop_back();
                }
              }
              chunk
            }
            Ok(Err(errno)) => {
              self.queue.pop_front();
              return Err(errno);
            }
            Err(_) => {
              self.queue.pop_front();
              return Err(EIO);
            }
          }
        }
      };

      let chunk_end = chunk.end();
      if cursor >= chunk_end {
        // We skipped past this chunk (forward skip or out-of-order readahead).
        // Save it in the backward seek window before popping so any delayed
        // lower-offset readahead request still hits RAM at 0 RTT!
        self.queue.pop_front();
        self.push_backward(chunk);
        if self.eof_offset.is_some_and(|eof| cursor >= eof) {
          break;
        }
        continue;
      }

      if cursor < chunk.offset {
        // Should not happen unless an earlier chunk was unexpectedly short;
        // reset to `cursor` and fetch directly.
        self.reset(cursor);
        continue;
      }

      let slice = chunk.slice_from(cursor, remaining).unwrap_or_default();
      let take = slice.len();
      if take == 0 {
        break;
      }
      cursor += take as u64;
      remaining -= take;

      if let Some(buf) = &mut combined {
        buf.extend_from_slice(&slice);
      } else if let Some(prev) = first.take() {
        let mut buf = BytesMut::with_capacity(prev.len() + take);
        buf.extend_from_slice(&prev);
        buf.extend_from_slice(&slice);
        combined = Some(buf);
      } else {
        first = Some(slice);
      }

      // If we consumed up to the end of this chunk, retire it into the backward
      // window and immediately top up the 2-stage pipeline!
      if cursor >= chunk_end {
        self.queue.pop_front();
        self.push_backward(chunk);
        self.fill_pipeline(source, remaining);
      }
    }

    if let Some(buf) = combined {
      Ok(buf.freeze())
    } else {
      Ok(first.unwrap_or_default())
    }
  }
}
