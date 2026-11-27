//! Multiplexed async RPC client over an `FsService::Session` bidirectional stream.
//!
//! Each outbound [`SessionRequest`] receives a monotonic `id` correlated to a
//! `oneshot` waiter. When the underlying stream closes, errors, or delivers an
//! [`FsForceUnmount`], all in-flight waiters fail immediately with `EIO` (5)
//! and an internal [`CancellationToken`] is cancelled so any mounted FUSE
//! session unmounts cleanly instead of hanging.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::{Stream, StreamExt as _, pin_mut};
use needy_api_proto::v1::{
  FsAttr, FsCreate, FsCreateAndWrite, FsCreated, FsData, FsEntry, FsFlush, FsForget, FsForgetItem, FsFsync, FsGetAttr, FsGetLock, FsLink,
  FsLockRange, FsLookup, FsMkdir, FsMount, FsMounted, FsOpen, FsOpened, FsPing, FsPong, FsRead, FsReadDirPlus, FsReadDirPlusReply,
  FsReadLink, FsRelease, FsRename, FsRmdir, FsSetAttr, FsSetLock, FsStatFs, FsStatFsReply, FsSymlink, FsUnlink, FsWrite, FsWritten,
  SessionRequest, SessionResponse, session_request, session_response,
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// Standard Linux/POSIX `EIO` errno value used for fail-fast transport errors.
pub const EIO: i32 = 5;
/// Standard Linux/POSIX `EPROTO` errno value for unexpected response variants.
pub const EPROTO: i32 = 71;

type PendingMap = Mutex<HashMap<u64, oneshot::Sender<Result<SessionResponse, i32>>>>;

/// Multiplexed async RPC client for a single `FsService::Session` stream.
pub struct FsRpcClient {
  tx: mpsc::Sender<SessionRequest>,
  next_id: AtomicU64,
  pending: Arc<PendingMap>,
  cancel: CancellationToken,
  force_unmount_reason: Arc<Mutex<Option<String>>>,
}

impl FsRpcClient {
  /// Constructs a multiplexed client over an outbound request channel and an
  /// inbound `SessionResponse` stream, spawning a background demuxer task.
  pub fn from_stream<S>(tx: mpsc::Sender<SessionRequest>, rx: S) -> Arc<Self>
  where
    S: Stream<Item = Result<SessionResponse, tonic::Status>> + Send + 'static,
  {
    let pending: Arc<PendingMap> = Arc::new(Mutex::new(HashMap::new()));
    let cancel = CancellationToken::new();
    let force_unmount_reason: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    let reader_pending = Arc::clone(&pending);
    let reader_cancel = cancel.clone();
    let reader_reason = Arc::clone(&force_unmount_reason);

    tokio::spawn(async move {
      pin_mut!(rx);
      while let Some(item) = rx.next().await {
        match item {
          Ok(resp) => {
            if let Some(session_response::Body::ForceUnmount(fu)) = resp.body {
              tracing::warn!("fs session force-unmounted: {}", fu.reason);
              if let Ok(mut guard) = reader_reason.lock() {
                *guard = Some(fu.reason);
              }
              fail_all_and_cancel(&reader_pending, &reader_cancel, EIO);
              return;
            }
            let waiter = reader_pending.lock().ok().and_then(|mut map| map.remove(&resp.id));
            if let Some(waiter) = waiter {
              let result = match resp.body {
                Some(session_response::Body::Error(err)) => {
                  let code = if err.errno != 0 { err.errno } else { EIO };
                  Err(code)
                }
                _ => Ok(resp),
              };
              // Caller may have been dropped; nothing to do if send fails.
              let _ = waiter.send(result);
            }
          }
          Err(status) => {
            tracing::warn!("fs session stream error: {status}");
            fail_all_and_cancel(&reader_pending, &reader_cancel, EIO);
            return;
          }
        }
      }
      tracing::debug!("fs session stream closed");
      fail_all_and_cancel(&reader_pending, &reader_cancel, EIO);
    });

    Arc::new(Self {
      tx,
      next_id: AtomicU64::new(1),
      pending,
      cancel,
      force_unmount_reason,
    })
  }

  /// Returns a [`CancellationToken`] that fires when the session stream closes,
  /// errors, or receives `FsForceUnmount`.
  pub fn cancelled(&self) -> CancellationToken {
    self.cancel.clone()
  }

  /// Returns whether the session has been cancelled/disconnected.
  pub fn is_cancelled(&self) -> bool {
    self.cancel.is_cancelled()
  }

  /// Returns the `FsForceUnmount` reason if the server forced an unmount.
  pub fn force_unmount_reason(&self) -> Option<String> {
    self.force_unmount_reason.lock().ok().and_then(|g| g.clone())
  }

  /// Sends a request and awaits its correlated response body.
  async fn call(&self, body: session_request::Body) -> Result<session_response::Body, i32> {
    if self.cancel.is_cancelled() {
      return Err(EIO);
    }

    let id = self.next_id.fetch_add(1, Ordering::Relaxed);
    let (resp_tx, resp_rx) = oneshot::channel();

    {
      let mut map = self.pending.lock().map_err(|_| EIO)?;
      // Re-check under the lock so a concurrent `fail_all_and_cancel` cannot
      // miss this waiter and leave it stranded.
      if self.cancel.is_cancelled() {
        return Err(EIO);
      }
      map.insert(id, resp_tx);
    }

    let req = SessionRequest {
      id,
      body: Some(body),
    };
    if self.tx.send(req).await.is_err() {
      fail_all_and_cancel(&self.pending, &self.cancel, EIO);
      return Err(EIO);
    }

    let resp = resp_rx.await.map_err(|_| EIO)??;
    resp.body.ok_or(EPROTO)
  }

  /// Handshakes and mounts a workspace root on the session.
  pub async fn mount(&self, req: FsMount) -> Result<FsMounted, i32> {
    match self.call(session_request::Body::Mount(req)).await? {
      session_response::Body::Mounted(m) => Ok(m),
      _ => Err(EPROTO),
    }
  }

  /// Looks up a child entry by name under `parent`.
  pub async fn lookup(&self, parent: u64, name: String) -> Result<FsEntry, i32> {
    match self.call(session_request::Body::Lookup(FsLookup { parent, name })).await? {
      session_response::Body::Entry(entry) => Ok(entry),
      _ => Err(EPROTO),
    }
  }

  /// Fetches attributes for `ino` (optionally via open handle `fh`).
  pub async fn getattr(&self, ino: u64, fh: Option<u64>) -> Result<FsAttr, i32> {
    match self.call(session_request::Body::Getattr(FsGetAttr { ino, fh })).await? {
      session_response::Body::Attr(reply) => reply.attr.ok_or(EPROTO),
      _ => Err(EPROTO),
    }
  }

  /// Updates file attributes (mode, size, atime, mtime).
  pub async fn setattr(&self, req: FsSetAttr) -> Result<FsAttr, i32> {
    match self.call(session_request::Body::Setattr(req)).await? {
      session_response::Body::Attr(reply) => reply.attr.ok_or(EPROTO),
      _ => Err(EPROTO),
    }
  }

  /// Reads directory entries along with their attributes.
  pub async fn readdirplus(&self, ino: u64, offset: u64, max_entries: u32) -> Result<FsReadDirPlusReply, i32> {
    match self
      .call(session_request::Body::Readdirplus(FsReadDirPlus {
        ino,
        offset,
        max_entries,
      }))
      .await?
    {
      session_response::Body::Readdirplus(reply) => Ok(reply),
      _ => Err(EPROTO),
    }
  }

  /// Opens an existing file inode.
  pub async fn open(&self, ino: u64, flags: u32, writeback: bool) -> Result<FsOpened, i32> {
    match self
      .call(session_request::Body::Open(FsOpen {
        ino,
        flags,
        writeback,
      }))
      .await?
    {
      session_response::Body::Opened(opened) => Ok(opened),
      _ => Err(EPROTO),
    }
  }

  /// Reads up to `size` bytes at `offset` from an open file handle.
  pub async fn read(&self, ino: u64, fh: u64, offset: u64, size: u32) -> Result<FsData, i32> {
    match self
      .call(session_request::Body::Read(FsRead {
        ino,
        fh,
        offset,
        size,
      }))
      .await?
    {
      session_response::Body::Data(data) => Ok(data),
      _ => Err(EPROTO),
    }
  }

  /// Writes `data` at `offset` to an open file handle.
  pub async fn write(&self, ino: u64, fh: u64, offset: u64, data: Vec<u8>) -> Result<FsWritten, i32> {
    match self
      .call(session_request::Body::Write(FsWrite {
        ino,
        fh,
        offset,
        data,
      }))
      .await?
    {
      session_response::Body::Written(written) => Ok(written),
      _ => Err(EPROTO),
    }
  }

  /// Flushes a file handle on `close()`.
  pub async fn flush(&self, ino: u64, fh: u64, lock_owner: u64) -> Result<(), i32> {
    match self
      .call(session_request::Body::Flush(FsFlush {
        ino,
        fh,
        lock_owner,
      }))
      .await?
    {
      session_response::Body::Ok(_) => Ok(()),
      _ => Err(EPROTO),
    }
  }

  /// Synchronizes file data (and optionally metadata) to storage.
  pub async fn fsync(&self, ino: u64, fh: u64, datasync: bool) -> Result<(), i32> {
    match self.call(session_request::Body::Fsync(FsFsync { ino, fh, datasync })).await? {
      session_response::Body::Ok(_) => Ok(()),
      _ => Err(EPROTO),
    }
  }

  /// Releases an open file handle when its last reference closes.
  pub async fn release(&self, ino: u64, fh: u64, lock_owner: u64) -> Result<(), i32> {
    match self
      .call(session_request::Body::Release(FsRelease {
        ino,
        fh,
        lock_owner,
      }))
      .await?
    {
      session_response::Body::Ok(_) => Ok(()),
      _ => Err(EPROTO),
    }
  }

  /// Creates and opens a file in one RPC.
  pub async fn create(&self, req: FsCreate) -> Result<FsCreated, i32> {
    match self.call(session_request::Body::Create(req)).await? {
      session_response::Body::Created(created) => Ok(created),
      _ => Err(EPROTO),
    }
  }

  /// Compound RPC: creates (or truncates), writes, and closes a small file.
  pub async fn create_and_write(&self, req: FsCreateAndWrite) -> Result<FsEntry, i32> {
    match self.call(session_request::Body::CreateAndWrite(req)).await? {
      session_response::Body::Entry(entry) => Ok(entry),
      session_response::Body::Created(created) => Ok(FsEntry {
        attr: created.attr,
        generation: created.generation,
      }),
      session_response::Body::Ok(_) => Ok(FsEntry::default()),
      _ => Err(EPROTO),
    }
  }

  /// Creates a directory under `parent`.
  pub async fn mkdir(&self, parent: u64, name: String, mode: u32) -> Result<FsEntry, i32> {
    match self.call(session_request::Body::Mkdir(FsMkdir { parent, name, mode })).await? {
      session_response::Body::Entry(entry) => Ok(entry),
      _ => Err(EPROTO),
    }
  }

  /// Unlinks a file under `parent`.
  pub async fn unlink(&self, parent: u64, name: String) -> Result<(), i32> {
    match self.call(session_request::Body::Unlink(FsUnlink { parent, name })).await? {
      session_response::Body::Ok(_) => Ok(()),
      _ => Err(EPROTO),
    }
  }

  /// Removes an empty directory under `parent`.
  pub async fn rmdir(&self, parent: u64, name: String) -> Result<(), i32> {
    match self.call(session_request::Body::Rmdir(FsRmdir { parent, name })).await? {
      session_response::Body::Ok(_) => Ok(()),
      _ => Err(EPROTO),
    }
  }

  /// Renames an entry (supporting `renameat2` flags).
  pub async fn rename(&self, req: FsRename) -> Result<(), i32> {
    match self.call(session_request::Body::Rename(req)).await? {
      session_response::Body::Ok(_) => Ok(()),
      _ => Err(EPROTO),
    }
  }

  /// Creates a symbolic link under `parent`.
  pub async fn symlink(&self, parent: u64, name: String, link_target: String) -> Result<FsEntry, i32> {
    match self
      .call(session_request::Body::Symlink(FsSymlink {
        parent,
        name,
        link_target,
      }))
      .await?
    {
      session_response::Body::Entry(entry) => Ok(entry),
      _ => Err(EPROTO),
    }
  }

  /// Reads the target of a symbolic link.
  pub async fn readlink(&self, ino: u64) -> Result<String, i32> {
    match self.call(session_request::Body::Readlink(FsReadLink { ino })).await? {
      session_response::Body::LinkTarget(target) => Ok(target.target),
      _ => Err(EPROTO),
    }
  }

  /// Creates a hard link to `ino` at `(new_parent, new_name)`.
  pub async fn link(&self, ino: u64, new_parent: u64, new_name: String) -> Result<FsEntry, i32> {
    match self
      .call(session_request::Body::Link(FsLink {
        ino,
        new_parent,
        new_name,
      }))
      .await?
    {
      session_response::Body::Entry(entry) => Ok(entry),
      _ => Err(EPROTO),
    }
  }

  /// Fetches filesystem statistics.
  pub async fn statfs(&self, ino: u64) -> Result<FsStatFsReply, i32> {
    match self.call(session_request::Body::Statfs(FsStatFs { ino })).await? {
      session_response::Body::Statfs(reply) => Ok(reply),
      _ => Err(EPROTO),
    }
  }

  /// Notifies the server that the kernel forgot inode lookup references.
  ///
  /// Sent fire-and-forget without waiting for a response, matching FUSE's
  /// one-way `FORGET` semantics.
  pub async fn forget(&self, items: Vec<FsForgetItem>) -> Result<(), i32> {
    if items.is_empty() {
      return Ok(());
    }
    if self.cancel.is_cancelled() {
      return Err(EIO);
    }
    let id = self.next_id.fetch_add(1, Ordering::Relaxed);
    let req = SessionRequest {
      id,
      body: Some(session_request::Body::Forget(FsForget { items })),
    };
    self.tx.send(req).await.map_err(|_| EIO)
  }

  /// Tests for a conflicting POSIX byte-range lock.
  pub async fn getlock(&self, req: FsGetLock) -> Result<FsLockRange, i32> {
    match self.call(session_request::Body::Getlock(req)).await? {
      session_response::Body::Lock(reply) => reply.range.ok_or(EPROTO),
      _ => Err(EPROTO),
    }
  }

  /// Acquires, modifies, or releases a POSIX byte-range lock.
  pub async fn setlock(&self, req: FsSetLock) -> Result<(), i32> {
    match self.call(session_request::Body::Setlock(req)).await? {
      session_response::Body::Ok(_) => Ok(()),
      _ => Err(EPROTO),
    }
  }

  /// Sends a keepalive ping and awaits the matching pong.
  pub async fn ping(&self, timestamp_ms: i64) -> Result<FsPong, i32> {
    match self.call(session_request::Body::Ping(FsPing { timestamp_ms })).await? {
      session_response::Body::Pong(pong) => Ok(pong),
      _ => Err(EPROTO),
    }
  }
}

fn fail_all_and_cancel(pending: &PendingMap, cancel: &CancellationToken, errno: i32) {
  cancel.cancel();
  if let Ok(mut map) = pending.lock() {
    for (_, waiter) in map.drain() {
      // Waiter may already be dropped; ignore.
      let _ = waiter.send(Err(errno));
    }
  }
}
