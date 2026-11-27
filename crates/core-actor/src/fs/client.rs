//! Multiplexed async RPC client over an `FsService::Session` bidirectional stream.
//!
//! Each outbound [`SessionRequest`] receives a monotonic `id` correlated to a
//! `oneshot` waiter. When the underlying stream closes or errors:
//!
//! - Without a reconnect connector ([`FsRpcClient::from_stream`]), all
//!   in-flight waiters fail immediately with `EIO` (5) and the internal
//!   [`CancellationToken`] is cancelled so any mounted FUSE session unmounts.
//! - With a reconnect connector ([`FsRpcClient::with_connector`]), all
//!   in-flight waiters fail immediately with `EIO` (5) while the mount stays
//!   alive and a background loop reconnects and replays `FsMount` for up to
//!   the configured reconnect window (default [`DEFAULT_RECONNECT_WINDOW`]).
//!   Incoming calls wait for the reconnected stream; if `FsForceUnmount`
//!   arrives or the window expires, the cancellation token fires and the mount
//!   unmounts cleanly.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{Stream, StreamExt as _};
use needy_api_proto::v1::{
  FsAttr, FsCreate, FsCreateAndWrite, FsCreated, FsData, FsEntry, FsFlush, FsForget, FsForgetItem, FsFsync, FsGetAttr, FsGetLock, FsLink,
  FsLockRange, FsLookup, FsMkdir, FsMount, FsMounted, FsOpen, FsOpened, FsPing, FsPong, FsRead, FsReadDirPlus, FsReadDirPlusReply,
  FsReadLink, FsRelease, FsRename, FsRmdir, FsSetAttr, FsSetLock, FsStatFs, FsStatFsReply, FsSymlink, FsUnlink, FsWrite, FsWritten,
  SessionRequest, SessionResponse, session_request, session_response,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

/// Standard Linux/POSIX `EIO` errno value used for fail-fast transport errors.
pub const EIO: i32 = 5;
/// Standard Linux/POSIX `EPROTO` errno value for unexpected response variants.
pub const EPROTO: i32 = 71;

/// Default window for reconnecting a dropped `FsService.Session` stream.
// NOTICE: 2 minutes, matching `crate::sshfs::stream::RECONNECT_WINDOW` and
// `needy_daemon::runner::DISCONNECT_GRACE` (which this crate cannot name): a
// runner that reconnects within the grace window restores the mount; one that
// does not has already ended the run's processes.
pub const DEFAULT_RECONNECT_WINDOW: Duration = Duration::from_secs(120);

// NOTICE: retries start quickly for transient tunnel cuts and back off to 5 s
// so a reconnected runner is picked up promptly without tight polling.
const RETRY_MIN: Duration = Duration::from_millis(250);
const RETRY_MAX: Duration = Duration::from_secs(5);
const RECONNECT_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(15);

/// Boxed inbound `SessionResponse` stream returned by a [`SessionConnector`].
pub type SessionResponseStream = Pin<Box<dyn Stream<Item = Result<SessionResponse, tonic::Status>> + Send + 'static>>;

/// A freshly opened `FsService.Session` bidirectional stream pair.
pub type ConnectedSession = (mpsc::Sender<SessionRequest>, SessionResponseStream);

/// Async factory that opens a fresh `FsService.Session` stream on reconnect.
pub type SessionConnector = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Result<ConnectedSession, String>> + Send>> + Send + Sync>;

struct ReconnectConfig {
  connector: SessionConnector,
  mount_req: FsMount,
  window: Duration,
}

#[derive(Clone)]
enum StreamState {
  Connected {
    tx: mpsc::Sender<SessionRequest>,
    epoch: u64,
  },
  Reconnecting,
  Closed,
}

struct PendingState {
  epoch: u64,
  reconnecting: bool,
  waiters: HashMap<u64, oneshot::Sender<Result<SessionResponse, i32>>>,
}

type PendingMap = Mutex<PendingState>;

/// Multiplexed async RPC client for an `FsService::Session` stream, with
/// optional transparent reconnection across transient disconnects.
pub struct FsRpcClient {
  state_tx: watch::Sender<StreamState>,
  state_rx: watch::Receiver<StreamState>,
  next_id: Arc<AtomicU64>,
  epoch: Arc<AtomicU64>,
  pending: Arc<PendingMap>,
  cancel: CancellationToken,
  force_unmount_reason: Arc<Mutex<Option<String>>>,
  reconnectable: bool,
}

impl Drop for FsRpcClient {
  fn drop(&mut self) {
    self.cancel.cancel();
  }
}

impl FsRpcClient {
  /// Constructs a multiplexed client over an outbound request channel and an
  /// inbound `SessionResponse` stream, spawning a background demuxer task.
  ///
  /// When the stream closes or errors, all in-flight calls fail with `EIO` and
  /// [`Self::cancelled`] fires immediately without reconnecting.
  pub fn from_stream<S>(tx: mpsc::Sender<SessionRequest>, rx: S) -> Arc<Self>
  where
    S: Stream<Item = Result<SessionResponse, tonic::Status>> + Send + 'static,
  {
    Self::build(tx, Box::pin(rx), None)
  }

  /// Constructs a multiplexed client with transparent session reconnection.
  ///
  /// When the active stream ends unexpectedly (not `FsForceUnmount` and not
  /// explicit cancellation), in-flight calls fail with `EIO` while the client
  /// retries `connector` and replays `mount_req` with exponential backoff up to
  /// `reconnect_window`. Incoming calls wait until reconnection succeeds or
  /// the window expires.
  pub fn with_connector<S>(
    tx: mpsc::Sender<SessionRequest>,
    rx: S,
    connector: SessionConnector,
    mount_req: FsMount,
    reconnect_window: Duration,
  ) -> Arc<Self>
  where
    S: Stream<Item = Result<SessionResponse, tonic::Status>> + Send + 'static,
  {
    Self::build(
      tx,
      Box::pin(rx),
      Some(ReconnectConfig {
        connector,
        mount_req,
        window: reconnect_window,
      }),
    )
  }

  fn build(tx: mpsc::Sender<SessionRequest>, rx: SessionResponseStream, reconnect: Option<ReconnectConfig>) -> Arc<Self> {
    let reconnectable = reconnect.is_some();
    let pending: Arc<PendingMap> = Arc::new(Mutex::new(PendingState {
      epoch: 0,
      reconnecting: false,
      waiters: HashMap::new(),
    }));
    let cancel = CancellationToken::new();
    let force_unmount_reason: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let next_id = Arc::new(AtomicU64::new(1));
    let epoch = Arc::new(AtomicU64::new(0));
    let (state_tx, state_rx) = watch::channel(StreamState::Connected { tx, epoch: 0 });

    let reader_pending = Arc::clone(&pending);
    let reader_cancel = cancel.clone();
    let reader_reason = Arc::clone(&force_unmount_reason);
    let reader_state_tx = state_tx.clone();
    let reader_next_id = Arc::clone(&next_id);
    let reader_epoch = Arc::clone(&epoch);

    tokio::spawn(async move {
      run_session_loop(rx, reconnect, reader_pending, reader_cancel, reader_reason, reader_state_tx, reader_next_id, reader_epoch).await;
    });

    Arc::new(Self {
      state_tx,
      state_rx,
      next_id,
      epoch,
      pending,
      cancel,
      force_unmount_reason,
      reconnectable,
    })
  }

  /// Returns a [`CancellationToken`] that fires when the session terminates
  /// permanently (explicit cancellation, `FsForceUnmount`, or exhausted
  /// reconnect window).
  pub fn cancelled(&self) -> CancellationToken {
    self.cancel.clone()
  }

  /// Returns whether the session has been permanently cancelled/disconnected.
  pub fn is_cancelled(&self) -> bool {
    self.cancel.is_cancelled()
  }

  /// Returns the current session epoch, incremented on each successful
  /// reconnect.
  pub fn session_epoch(&self) -> u64 {
    self.epoch.load(Ordering::Acquire)
  }

  /// Returns the `FsForceUnmount` reason if the server forced an unmount.
  pub fn force_unmount_reason(&self) -> Option<String> {
    self.force_unmount_reason.lock().ok().and_then(|g| g.clone())
  }

  /// Acquires the active outbound sender, waiting through a transient
  /// reconnect if one is in progress.
  async fn active_sender(&self) -> Result<(mpsc::Sender<SessionRequest>, u64), i32> {
    let mut rx = self.state_rx.clone();
    loop {
      if self.cancel.is_cancelled() {
        return Err(EIO);
      }
      let state = rx.borrow_and_update().clone();
      match state {
        StreamState::Connected { tx, epoch } => return Ok((tx, epoch)),
        StreamState::Closed => return Err(EIO),
        StreamState::Reconnecting => {
          tokio::select! {
            changed = rx.changed() => {
              if changed.is_err() {
                return Err(EIO);
              }
            }
            () = self.cancel.cancelled() => return Err(EIO),
          }
        }
      }
    }
  }

  /// Sends a request and awaits its correlated response body.
  async fn call(&self, body: session_request::Body) -> Result<session_response::Body, i32> {
    loop {
      let (tx, epoch) = self.active_sender().await?;
      let id = self.next_id.fetch_add(1, Ordering::Relaxed);
      let (resp_tx, resp_rx) = oneshot::channel();

      {
        let mut state = self.pending.lock().map_err(|_| EIO)?;
        // Re-check under the lock so a concurrent disconnect or cancellation
        // cannot miss this waiter and leave it stranded.
        if self.cancel.is_cancelled() {
          return Err(EIO);
        }
        if state.reconnecting || state.epoch != epoch {
          // The stream we grabbed just disconnected; wait for the next
          // connected state (or permanent closure).
          continue;
        }
        state.waiters.insert(id, resp_tx);
      }

      let req = SessionRequest {
        id,
        body: Some(body),
      };
      if tx.send(req).await.is_err() {
        if self.reconnectable {
          if let Ok(mut state) = self.pending.lock() {
            state.waiters.remove(&id);
          }
        } else {
          fail_all_and_cancel(&self.pending, &self.cancel, &self.state_tx, EIO);
        }
        return Err(EIO);
      }

      let resp = resp_rx.await.map_err(|_| EIO)??;
      return resp.body.ok_or(EPROTO);
    }
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

  /// Prefetches a bounded BFS subtree of directory listings in one RPC.
  pub async fn prefetch_tree(
    &self,
    ino: u64,
    max_depth: u32,
    max_dirs: u32,
    skip_names: Vec<String>,
  ) -> Result<needy_api_proto::v1::FsPrefetchTreeReply, i32> {
    match self
      .call(session_request::Body::PrefetchTree(needy_api_proto::v1::FsPrefetchTree {
        ino,
        max_depth,
        max_dirs,
        skip_names,
      }))
      .await?
    {
      session_response::Body::PrefetchTree(reply) => Ok(reply),
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
    let tx = match self.state_rx.borrow().clone() {
      StreamState::Connected { tx, .. } => tx,
      StreamState::Reconnecting | StreamState::Closed => return Err(EIO),
    };
    let id = self.next_id.fetch_add(1, Ordering::Relaxed);
    let req = SessionRequest {
      id,
      body: Some(session_request::Body::Forget(FsForget { items })),
    };
    tx.send(req).await.map_err(|_| EIO)
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

enum AttemptResult {
  Connected(mpsc::Sender<SessionRequest>, SessionResponseStream),
  ForceUnmount,
  Retry(String),
}

#[allow(clippy::too_many_arguments)]
async fn run_session_loop(
  mut rx: SessionResponseStream,
  reconnect: Option<ReconnectConfig>,
  pending: Arc<PendingMap>,
  cancel: CancellationToken,
  reason: Arc<Mutex<Option<String>>>,
  state_tx: watch::Sender<StreamState>,
  next_id: Arc<AtomicU64>,
  epoch_counter: Arc<AtomicU64>,
) {
  loop {
    let disconnect_reason = loop {
      let item = tokio::select! {
        item = rx.next() => item,
        () = cancel.cancelled() => {
          fail_all_and_cancel(&pending, &cancel, &state_tx, EIO);
          return;
        }
      };
      match item {
        Some(Ok(resp)) => {
          if let Some(session_response::Body::ForceUnmount(fu)) = resp.body {
            tracing::warn!("fs session force-unmounted: {}", fu.reason);
            if let Ok(mut guard) = reason.lock() {
              *guard = Some(fu.reason);
            }
            fail_all_and_cancel(&pending, &cancel, &state_tx, EIO);
            return;
          }
          let waiter = pending.lock().ok().and_then(|mut state| state.waiters.remove(&resp.id));
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
        Some(Err(status)) => {
          break format!("stream error: {status}");
        }
        None => {
          break "stream closed".to_string();
        }
      }
    };

    let Some(cfg) = &reconnect else {
      tracing::debug!("fs session disconnected ({disconnect_reason})");
      fail_all_and_cancel(&pending, &cancel, &state_tx, EIO);
      return;
    };

    tracing::warn!("fs session lost ({disconnect_reason}); reconnecting for up to {:?}", cfg.window);
    begin_reconnect(&pending, &state_tx, EIO);

    match reconnect_session(cfg, &cancel, &next_id, &reason).await {
      Some((new_tx, new_rx)) => {
        let new_epoch = {
          let mut state = pending.lock().unwrap_or_else(|e| e.into_inner());
          state.epoch += 1;
          state.reconnecting = false;
          state.epoch
        };
        epoch_counter.store(new_epoch, Ordering::Release);
        tracing::info!("fs session reconnected (epoch {new_epoch})");
        state_tx.send_replace(StreamState::Connected {
          tx: new_tx,
          epoch: new_epoch,
        });
        rx = new_rx;
      }
      None => {
        fail_all_and_cancel(&pending, &cancel, &state_tx, EIO);
        return;
      }
    }
  }
}

async fn reconnect_session(
  cfg: &ReconnectConfig,
  cancel: &CancellationToken,
  next_id: &AtomicU64,
  reason: &Mutex<Option<String>>,
) -> Option<ConnectedSession> {
  let deadline = tokio::time::Instant::now() + cfg.window;
  let mut delay = RETRY_MIN;
  loop {
    let now = tokio::time::Instant::now();
    let remaining = deadline.saturating_duration_since(now);
    let attempt_timeout = remaining.min(RECONNECT_ATTEMPT_TIMEOUT);

    let outcome = tokio::select! {
      () = cancel.cancelled() => return None,
      res = tokio::time::timeout(attempt_timeout, attempt_reconnect(cfg, next_id, reason)) => {
        match res {
          Ok(outcome) => outcome,
          Err(_) => AttemptResult::Retry("reconnect attempt timed out".into()),
        }
      }
    };

    match outcome {
      AttemptResult::Connected(tx, rx) => return Some((tx, rx)),
      AttemptResult::ForceUnmount => return None,
      AttemptResult::Retry(err) => {
        tracing::debug!("fs session reconnect attempt failed: {err}");
      }
    }

    let now = tokio::time::Instant::now();
    if now >= deadline {
      tracing::warn!("fs session did not reconnect within {:?}; unmounting", cfg.window);
      return None;
    }
    let sleep_for = delay.min(deadline - now);
    tokio::select! {
      () = cancel.cancelled() => return None,
      () = tokio::time::sleep(sleep_for) => {}
    }
    delay = (delay * 2).min(RETRY_MAX);
  }
}

async fn attempt_reconnect(cfg: &ReconnectConfig, next_id: &AtomicU64, reason: &Mutex<Option<String>>) -> AttemptResult {
  let (tx, mut rx) = match (cfg.connector)().await {
    Ok(pair) => pair,
    Err(err) => return AttemptResult::Retry(err),
  };

  let mount_id = next_id.fetch_add(1, Ordering::Relaxed);
  let req = SessionRequest {
    id: mount_id,
    body: Some(session_request::Body::Mount(cfg.mount_req.clone())),
  };
  if tx.send(req).await.is_err() {
    return AttemptResult::Retry("request channel closed before FsMount".into());
  }

  match rx.next().await {
    Some(Ok(resp)) => match resp.body {
      Some(session_response::Body::ForceUnmount(fu)) => {
        tracing::warn!("fs session force-unmounted during reconnect: {}", fu.reason);
        if let Ok(mut guard) = reason.lock() {
          *guard = Some(fu.reason);
        }
        AttemptResult::ForceUnmount
      }
      Some(session_response::Body::Mounted(_)) if resp.id == mount_id => AttemptResult::Connected(tx, rx),
      Some(session_response::Body::Error(err)) => AttemptResult::Retry(format!("FsMount failed (errno {}): {}", err.errno, err.message)),
      _ => AttemptResult::Retry("unexpected response to replayed FsMount".into()),
    },
    Some(Err(status)) => AttemptResult::Retry(format!("stream error during FsMount: {status}")),
    None => AttemptResult::Retry("stream closed before FsMounted".into()),
  }
}

fn begin_reconnect(pending: &PendingMap, state_tx: &watch::Sender<StreamState>, errno: i32) {
  state_tx.send_replace(StreamState::Reconnecting);
  if let Ok(mut state) = pending.lock() {
    state.reconnecting = true;
    for (_, waiter) in state.waiters.drain() {
      // Waiter may already be dropped; ignore.
      let _ = waiter.send(Err(errno));
    }
  }
}

fn fail_all_and_cancel(pending: &PendingMap, cancel: &CancellationToken, state_tx: &watch::Sender<StreamState>, errno: i32) {
  cancel.cancel();
  state_tx.send_replace(StreamState::Closed);
  if let Ok(mut state) = pending.lock() {
    state.reconnecting = false;
    for (_, waiter) in state.waiters.drain() {
      // Waiter may already be dropped; ignore.
      let _ = waiter.send(Err(errno));
    }
  }
}
