//! Linux FUSE3 `raw::Filesystem` implementation backed by [`FsRpcClient`].

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use bytes::Bytes;
use fuse3::raw::reply::{
  DirectoryEntryPlus, FileAttr, ReplyAttr, ReplyCreated, ReplyData, ReplyDirectoryPlus, ReplyEntry, ReplyInit, ReplyLock, ReplyOpen,
  ReplyStatFs, ReplyWrite,
};
use fuse3::raw::{Filesystem, Request, Session};
use fuse3::{Errno, FileType, Inode, MountOptions, Result as FuseResult, SetAttr, Timestamp};
use futures_util::stream::{self, Stream};
use needy_api_proto::v1::{
  FsAttr, FsCreate, FsDirEntry, FsFileType, FsForgetItem, FsGetLock, FsLockMode, FsLockRange, FsLockType, FsMount, FsRename, FsSetAttr,
  FsSetLock, SessionRequest, SessionResponse,
};
use tokio::sync::{Mutex, Notify, Semaphore, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::client::FsRpcClient;
use super::neg_cache::{DEFAULT_CAPACITY, NegativeCache};
use super::prefetch::Prefetcher;
use super::write_behind::WriteBehind;
use super::{MountConfig, detach};

/// Maximum write size advertised in `FUSE_INIT` (1 MiB).
const MAX_WRITE_BYTES: u32 = 1024 * 1024;
/// Maximum concurrent background FUSE requests (readahead, async writes).
const MAX_BACKGROUND_REQUESTS: u16 = 64;
/// Congestion threshold (75% of `MAX_BACKGROUND_REQUESTS`).
const CONGESTION_THRESHOLD: u16 = 48;
/// Page size used when fetching `ReadDirPlus` batches from the executor.
const READDIRPLUS_BATCH_SIZE: u32 = 512;
/// Maximum directory depth requested in a single batched `PrefetchTree` RPC.
const PREFETCH_TREE_MAX_DEPTH: u32 = 5;
/// Maximum directories requested in a single batched `PrefetchTree` RPC.
const PREFETCH_TREE_MAX_DIRS: u32 = 3500;
/// Fallback depth when the executor does not support `PrefetchTree`.
const PREFETCH_FALLBACK_MAX_DEPTH: usize = 2;
/// Maximum concurrent `ReadDirPlus` RPCs during fallback metadata prefetch.
const PREFETCH_CONCURRENCY: usize = 16;
/// Maximum number of directories warmed during fallback prefetch.
const PREFETCH_FALLBACK_MAX_DIRS: usize = 256;

/// Directory names skipped during background metadata prefetch to avoid
/// warming huge dependency or VCS trees.
const PREFETCH_SKIP_DIRS: &[&str] = &[
  ".git",
  "node_modules",
  "target",
  "dist",
  "build",
  ".next",
  ".cache",
  "__pycache__",
  ".venv",
  "vendor",
];

struct DirCacheEntry {
  entries: Vec<FsDirEntry>,
  /// Per-entry flag parallel to `entries`: `true` when this entry carries an
  /// unconsumed server `nlookup = 1` reference from prefetch that may be
  /// handed 1:1 to the FUSE kernel on either `lookup` or `readdirplus(0)`, or
  /// returned via `Forget` if evicted unused.
  ref_available: Vec<bool>,
  fetched_at: Instant,
}

struct DirNamesEntry {
  names: HashSet<String>,
  fetched_at: Instant,
}

#[derive(Default)]
struct DirCacheInner {
  entries: HashMap<u64, DirCacheEntry>,
  names: HashMap<u64, DirNamesEntry>,
  in_flight: HashMap<u64, Arc<Notify>>,
}

struct DirCache {
  inner: Mutex<DirCacheInner>,
}

impl DirCache {
  fn new() -> Self {
    Self {
      inner: Mutex::new(DirCacheInner::default()),
    }
  }

  fn normalize_name(name: &str, case_insensitive: bool) -> String {
    if case_insensitive {
      name.to_lowercase()
    } else {
      name.to_owned()
    }
  }

  fn collect_forget_items(entry: DirCacheEntry) -> Vec<FsForgetItem> {
    entry
      .entries
      .into_iter()
      .zip(entry.ref_available)
      .filter(|(e, avail)| *avail && e.ino > 1)
      .map(|(e, _)| FsForgetItem {
        ino: e.ino,
        nlookup: 1,
      })
      .collect()
  }

  async fn forget_items(client: &FsRpcClient, items: Vec<FsForgetItem>) {
    if !items.is_empty() {
      let _ = client.forget(items).await;
    }
  }

  /// Attempts a 0-RTT positive lookup from a fresh prefetched directory entry,
  /// handing off that entry's single `nlookup = 1` reference to the kernel.
  async fn take_prefetched_lookup(&self, parent: u64, name: &str, case_insensitive: bool, ttl: Duration) -> Option<FsDirEntry> {
    let mut inner = self.inner.lock().await;
    let dir = inner.entries.get_mut(&parent)?;
    if dir.fetched_at.elapsed() >= ttl {
      return None;
    }
    let idx = dir.entries.iter().position(|e| {
      if case_insensitive {
        e.name.eq_ignore_ascii_case(name)
      } else {
        e.name == name
      }
    })?;
    if !dir.ref_available[idx] {
      return None;
    }
    dir.ref_available[idx] = false;
    Some(dir.entries[idx].clone())
  }

  /// Inserts a batch of prefetched directories returned by `PrefetchTree`.
  async fn insert_prefetched_tree(&self, client: &FsRpcClient, dirs: Vec<needy_api_proto::v1::FsPrefetchDir>, case_insensitive: bool) {
    let stale_forget = {
      let mut inner = self.inner.lock().await;
      let now = Instant::now();
      let mut to_forget = Vec::new();
      for dir in dirs {
        let names: HashSet<String> = dir.entries.iter().map(|e| Self::normalize_name(&e.name, case_insensitive)).collect();
        inner.names.insert(
          dir.ino,
          DirNamesEntry {
            names,
            fetched_at: now,
          },
        );
        let ref_available = vec![true; dir.entries.len()];
        if let Some(prev) = inner.entries.insert(
          dir.ino,
          DirCacheEntry {
            entries: dir.entries,
            ref_available,
            fetched_at: now,
          },
        ) {
          to_forget.extend(Self::collect_forget_items(prev));
        }
      }
      to_forget
    };
    Self::forget_items(client, stale_forget).await;
  }

  /// Returns `true` when we hold a fresh complete directory listing for
  /// `parent` that does not contain `name` (0-RTT negative lookup without
  /// touching any server inode refcounts).
  async fn definitely_missing(&self, parent: u64, name: &str, case_insensitive: bool, ttl: Duration) -> bool {
    let inner = self.inner.lock().await;
    let Some(entry) = inner.names.get(&parent) else {
      return false;
    };
    if entry.fetched_at.elapsed() >= ttl {
      return false;
    }
    let key = Self::normalize_name(name, case_insensitive);
    !entry.names.contains(&key)
  }

  async fn invalidate_dir(&self, client: &FsRpcClient, parent: u64) {
    let to_forget = {
      let mut inner = self.inner.lock().await;
      inner.names.remove(&parent);
      inner.entries.remove(&parent).map(Self::collect_forget_items).unwrap_or_default()
    };
    Self::forget_items(client, to_forget).await;
  }

  /// Releases pagination entries after `releasedir`, preserving `names` for
  /// negative lookups and preserving directories that still hold unconsumed
  /// prefetched entries.
  async fn release_dir(&self, parent: u64) {
    let mut inner = self.inner.lock().await;
    if inner.entries.get(&parent).is_some_and(|e| e.ref_available.iter().all(|a| !a)) {
      inner.entries.remove(&parent);
    }
  }

  async fn clear(&self, client: &FsRpcClient) {
    let to_forget = {
      let mut inner = self.inner.lock().await;
      inner.names.clear();
      let mut items = Vec::new();
      for (_, entry) in inner.entries.drain() {
        items.extend(Self::collect_forget_items(entry));
      }
      items
    };
    Self::forget_items(client, to_forget).await;
  }

  /// Fetches or reuses a directory listing for `parent`.
  ///
  /// When `is_prefetch` is `false` and `offset == 0`, an unconsumed prefetched
  /// snapshot (all entries still holding `nlookup = 1`) is handed off 1:1 to
  /// the FUSE kernel. Subsequent `offset > 0` pages reuse the same in-flight
  /// `readdirplus` snapshot. Concurrent callers for the same `parent` coalesce
  /// on a single RPC.
  async fn load_entries(
    &self,
    client: &FsRpcClient,
    parent: u64,
    offset: u64,
    case_insensitive: bool,
    ttl: Duration,
    is_prefetch: bool,
  ) -> FuseResult<Vec<FsDirEntry>> {
    loop {
      let wait_notify = {
        let mut inner = self.inner.lock().await;
        if let Some(existing) = inner.entries.get_mut(&parent) {
          if existing.fetched_at.elapsed() < ttl {
            if is_prefetch {
              return Ok(existing.entries.clone());
            }
            let all_available = existing.ref_available.iter().all(|a| *a);
            let none_available = existing.ref_available.iter().all(|a| !a);
            if all_available && offset == 0 {
              // Hand off all prefetched nlookup=1 references to the kernel.
              existing.ref_available.fill(false);
              return Ok(existing.entries.clone());
            }
            if none_available && offset > 0 {
              return Ok(existing.entries.clone());
            }
          }
        }
        if let Some(notify) = inner.in_flight.get(&parent) {
          Some(Arc::clone(notify))
        } else {
          let notify = Arc::new(Notify::new());
          inner.in_flight.insert(parent, notify);
          None
        }
      };

      if let Some(notify) = wait_notify {
        notify.notified().await;
        continue;
      }

      let fetch_result: FuseResult<Vec<FsDirEntry>> = async {
        let mut all_entries = Vec::new();
        let mut cursor = 0u64;
        loop {
          let reply = client.readdirplus(parent, cursor, READDIRPLUS_BATCH_SIZE).await.map_err(Errno::from)?;
          let last_offset = reply.entries.last().map(|e| e.offset);
          all_entries.extend(reply.entries);
          if reply.eof {
            break;
          }
          match last_offset {
            Some(next) if next > cursor => cursor = next,
            _ => break,
          }
        }
        Ok(all_entries)
      }
      .await;

      let (stale_forget, notify) = {
        let mut inner = self.inner.lock().await;
        let notify = inner.in_flight.remove(&parent);
        let stale_forget = match &fetch_result {
          Ok(all_entries) => {
            let now = Instant::now();
            let names: HashSet<String> = all_entries.iter().map(|e| Self::normalize_name(&e.name, case_insensitive)).collect();
            inner.names.insert(
              parent,
              DirNamesEntry {
                names,
                fetched_at: now,
              },
            );
            let ref_available = vec![is_prefetch; all_entries.len()];
            inner
              .entries
              .insert(
                parent,
                DirCacheEntry {
                  entries: all_entries.clone(),
                  ref_available,
                  fetched_at: now,
                },
              )
              .map(Self::collect_forget_items)
              .unwrap_or_default()
          }
          Err(_) => Vec::new(),
        };
        (stale_forget, notify)
      };

      if let Some(notify) = notify {
        notify.notify_waiters();
      }
      Self::forget_items(client, stale_forget).await;
      return fetch_result;
    }
  }
}

/// Spawns a bounded metadata prefetcher right after mount initialization.
///
/// Prefers a single batched `PrefetchTree` RPC (warming up to 5 levels / 3,500
/// directories in one round trip) and falls back to concurrent per-directory
/// `ReadDirPlus` against older executors.
fn spawn_metadata_prefetch(client: Arc<FsRpcClient>, dir_cache: Arc<DirCache>, case_insensitive: bool, ttl: Duration) {
  let cancel = client.cancelled();
  tokio::spawn(async move {
    if cancel.is_cancelled() {
      return;
    }
    let skip_names: Vec<String> = PREFETCH_SKIP_DIRS.iter().map(|&s| s.to_owned()).collect();
    match client.prefetch_tree(1, PREFETCH_TREE_MAX_DEPTH, PREFETCH_TREE_MAX_DIRS, skip_names).await {
      Ok(reply) => {
        if !cancel.is_cancelled() {
          dir_cache.insert_prefetched_tree(&client, reply.dirs, case_insensitive).await;
        }
        return;
      }
      Err(errno) => {
        tracing::debug!("PrefetchTree unavailable (errno {errno}); falling back to shallow per-dir prefetch");
      }
    }

    let sem = Arc::new(Semaphore::new(PREFETCH_CONCURRENCY));
    let fetched = Arc::new(AtomicUsize::new(0));
    let mut frontier: Vec<u64> = vec![1];

    for depth in 0..=PREFETCH_FALLBACK_MAX_DEPTH {
      if frontier.is_empty() || cancel.is_cancelled() || fetched.load(Ordering::Relaxed) >= PREFETCH_FALLBACK_MAX_DIRS {
        break;
      }
      let mut handles = Vec::with_capacity(frontier.len());
      for dir_ino in frontier.drain(..) {
        if cancel.is_cancelled() || fetched.fetch_add(1, Ordering::Relaxed) >= PREFETCH_FALLBACK_MAX_DIRS {
          break;
        }
        let sem = Arc::clone(&sem);
        let client = Arc::clone(&client);
        let dir_cache = Arc::clone(&dir_cache);
        let cancel = cancel.clone();
        handles.push(tokio::spawn(async move {
          let Ok(_permit) = sem.acquire_owned().await else {
            return Vec::new();
          };
          if cancel.is_cancelled() {
            return Vec::new();
          }
          let Ok(entries) = dir_cache.load_entries(&client, dir_ino, 0, case_insensitive, ttl, true).await else {
            return Vec::new();
          };
          if depth >= PREFETCH_FALLBACK_MAX_DEPTH {
            return Vec::new();
          }
          entries
            .into_iter()
            .filter_map(|e| {
              let kind = e.attr.as_ref().and_then(|a| FsFileType::try_from(a.kind).ok())?;
              if kind == FsFileType::Directory && e.ino > 1 && !PREFETCH_SKIP_DIRS.contains(&e.name.as_str()) {
                Some(e.ino)
              } else {
                None
              }
            })
            .collect::<Vec<u64>>()
        }));
      }

      let mut next_frontier = Vec::new();
      for handle in handles {
        if let Ok(children) = handle.await {
          next_frontier.extend(children);
        }
      }
      frontier = next_frontier;
    }
  });
}

#[derive(Debug, Clone, Copy)]
struct LocalLock {
  owner: u64,
  start: u64,
  end: u64,
  kind: u32,
  pid: u32,
}

fn ranges_overlap(s1: u64, e1: u64, s2: u64, e2: u64) -> bool {
  s1 <= e2 && s2 <= e1
}

/// Removes `[start, end]` from `owner`'s locks in `entries`, splitting any
/// partially overlapping lock. Returns `true` if any lock was modified.
fn remove_owner_range(entries: &mut Vec<LocalLock>, owner: u64, start: u64, end: u64) -> bool {
  let mut changed = false;
  let mut updated = Vec::with_capacity(entries.len());
  for lock in entries.drain(..) {
    if lock.owner != owner || !ranges_overlap(lock.start, lock.end, start, end) {
      updated.push(lock);
      continue;
    }
    changed = true;
    if lock.start < start {
      updated.push(LocalLock {
        end: start - 1,
        ..lock
      });
    }
    if lock.end > end {
      updated.push(LocalLock {
        start: end + 1,
        ..lock
      });
    }
  }
  *entries = updated;
  changed
}

/// In-process POSIX advisory byte-range lock table for `FsLockMode::Local`.
///
/// NOTICE: `fuse3` 0.10.0 unconditionally advertises `FUSE_POSIX_LOCKS` in
/// `FUSE_INIT` (`fuse3-0.10.0/src/raw/session.rs`), so the Linux kernel always
/// routes `fcntl(F_GETLK/F_SETLK/F_SETLKW)` to the FUSE daemon instead of
/// handling locks in the VFS. Returning `ENOSYS` breaks SQLite (`disk I/O
/// error`). Handling `FsLockMode::Local` here gives full POSIX advisory locking
/// across actor processes with zero network round-trips.
#[derive(Default)]
struct LocalLockTable {
  locks: std::sync::Mutex<HashMap<u64, Vec<LocalLock>>>,
  notify: Notify,
}

impl LocalLockTable {
  fn getlk(&self, inode: u64, owner: u64, start: u64, end: u64, kind: u32) -> ReplyLock {
    let map = self.locks.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(entries) = map.get(&inode)
      && let Some(conflict) = entries.iter().find(|l| {
        l.owner != owner && ranges_overlap(l.start, l.end, start, end) && (l.kind == libc::F_WRLCK as u32 || kind == libc::F_WRLCK as u32)
      })
    {
      return ReplyLock {
        start: conflict.start,
        end: conflict.end,
        r#type: conflict.kind,
        pid: conflict.pid,
      };
    }
    ReplyLock {
      start,
      end,
      r#type: libc::F_UNLCK as u32,
      pid: 0,
    }
  }

  async fn setlk(&self, inode: u64, lock: LocalLock, block: bool) -> FuseResult<()> {
    let LocalLock {
      owner,
      start,
      end,
      kind,
      pid,
    } = lock;
    if kind == libc::F_UNLCK as u32 {
      let changed = {
        let mut map = self.locks.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entries) = map.get_mut(&inode) {
          let c = remove_owner_range(entries, owner, start, end);
          if entries.is_empty() {
            map.remove(&inode);
          }
          c
        } else {
          false
        }
      };
      if changed {
        self.notify.notify_waiters();
      }
      return Ok(());
    }

    loop {
      let notified = self.notify.notified();
      tokio::pin!(notified);
      notified.as_mut().enable();
      {
        let mut map = self.locks.lock().unwrap_or_else(|e| e.into_inner());
        let entries = map.entry(inode).or_default();
        let conflict = entries.iter().any(|l| {
          l.owner != owner && ranges_overlap(l.start, l.end, start, end) && (l.kind == libc::F_WRLCK as u32 || kind == libc::F_WRLCK as u32)
        });
        if !conflict {
          let changed = remove_owner_range(entries, owner, start, end);
          entries.push(LocalLock {
            owner,
            start,
            end,
            kind,
            pid,
          });
          drop(map);
          if changed {
            self.notify.notify_waiters();
          }
          return Ok(());
        }
      }
      if !block {
        return Err(Errno::from(libc::EAGAIN));
      }
      notified.await;
    }
  }

  fn unlock_owner(&self, inode: u64, owner: u64) {
    let changed = {
      let mut map = self.locks.lock().unwrap_or_else(|e| e.into_inner());
      if let Some(entries) = map.get_mut(&inode) {
        let before = entries.len();
        entries.retain(|l| l.owner != owner);
        let removed = entries.len() != before;
        if entries.is_empty() {
          map.remove(&inode);
        }
        removed
      } else {
        false
      }
    };
    if changed {
      self.notify.notify_waiters();
    }
  }
}

/// Async FUSE3 filesystem backed by an [`FsRpcClient`] session.
pub struct NeedyFs {
  client: Arc<FsRpcClient>,
  config: MountConfig,
  last_epoch: AtomicU64,
  neg_cache: NegativeCache,
  prefetcher: Prefetcher<FsRpcClient>,
  write_behind: WriteBehind<FsRpcClient>,
  dir_cache: Arc<DirCache>,
  local_locks: LocalLockTable,
}

impl NeedyFs {
  /// Constructs a [`NeedyFs`] instance from an already-mounted [`FsRpcClient`].
  pub fn new(client: Arc<FsRpcClient>, config: MountConfig) -> Self {
    let last_epoch = AtomicU64::new(client.session_epoch());
    let prefetcher = Prefetcher::new(Arc::clone(&client));
    let write_behind = WriteBehind::new(Arc::clone(&client));
    let neg_cache = NegativeCache::with_case_sensitivity(DEFAULT_CAPACITY, config.entry_ttl, config.case_insensitive);
    Self {
      client,
      config,
      last_epoch,
      neg_cache,
      prefetcher,
      write_behind,
      dir_cache: Arc::new(DirCache::new()),
      local_locks: LocalLockTable::default(),
    }
  }

  /// Clears client-side caches whose inode numbers or handles belong to an
  /// older `FsService` session after a transparent reconnect.
  // TODO(fs-reconnect-inodes): cached non-root directory inodes (`ino > 1`)
  // from before a reconnect are not remapped because `HostFs` allocates fresh
  // session-local inode numbers on each `FsMount`; root-relative lookups
  // (`ino = 1`) recover immediately. Re-open if a nested-cwd reconnect
  // scenario requires virtual inode remapping or kernel entry invalidation.
  async fn sync_epoch(&self) {
    let epoch = self.client.session_epoch();
    let prev = self.last_epoch.load(Ordering::Acquire);
    if epoch != prev && self.last_epoch.compare_exchange(prev, epoch, Ordering::AcqRel, Ordering::Acquire).is_ok() {
      self.neg_cache.clear();
      self.prefetcher.clear().await;
      self.dir_cache.clear(&self.client).await;
    }
  }

  fn to_file_attr(&self, attr: &FsAttr) -> FileAttr {
    let kind = match FsFileType::try_from(attr.kind).unwrap_or(FsFileType::Regular) {
      FsFileType::Directory => FileType::Directory,
      FsFileType::Symlink => FileType::Symlink,
      FsFileType::Regular | FsFileType::Unspecified => FileType::RegularFile,
    };
    FileAttr {
      ino: attr.ino,
      size: attr.size,
      blocks: attr.blocks,
      atime: Timestamp::new(attr.atime_sec, attr.atime_nsec),
      mtime: Timestamp::new(attr.mtime_sec, attr.mtime_nsec),
      ctime: Timestamp::new(attr.ctime_sec, attr.ctime_nsec),
      kind,
      perm: (attr.perm & 0o7777) as u16,
      nlink: attr.nlink,
      uid: self.config.uid,
      gid: self.config.gid,
      rdev: 0,
      blksize: if attr.blksize != 0 {
        attr.blksize
      } else {
        4096
      },
    }
  }

  async fn invalidate_dir(&self, parent: u64) {
    self.dir_cache.invalidate_dir(&self.client, parent).await;
  }

  async fn load_dir_entries(&self, parent: u64, offset: u64) -> FuseResult<Vec<FsDirEntry>> {
    self.sync_epoch().await;
    self.dir_cache.load_entries(&self.client, parent, offset, self.config.case_insensitive, self.config.entry_ttl, false).await
  }
}

fn os_str_to_string(s: &OsStr) -> FuseResult<String> {
  s.to_str().map(ToOwned::to_owned).ok_or_else(|| Errno::from(libc::EINVAL))
}

fn posix_lock_type_to_proto(kind: u32) -> FsLockType {
  match kind as libc::c_int {
    libc::F_RDLCK => FsLockType::Read,
    libc::F_WRLCK => FsLockType::Write,
    _ => FsLockType::Unlock,
  }
}

fn proto_lock_type_to_posix(kind: i32) -> u32 {
  match FsLockType::try_from(kind).unwrap_or(FsLockType::Unlock) {
    FsLockType::Read => libc::F_RDLCK as u32,
    FsLockType::Write => libc::F_WRLCK as u32,
    FsLockType::Unlock | FsLockType::Unspecified => libc::F_UNLCK as u32,
  }
}

impl Filesystem for NeedyFs {
  async fn init(&self, _req: Request) -> FuseResult<ReplyInit> {
    spawn_metadata_prefetch(Arc::clone(&self.client), Arc::clone(&self.dir_cache), self.config.case_insensitive, self.config.entry_ttl);
    let mut reply = ReplyInit::new(NonZeroU32::new(MAX_WRITE_BYTES).expect("non-zero"));
    reply.max_background = Some(MAX_BACKGROUND_REQUESTS);
    reply.congestion_threshold = Some(CONGESTION_THRESHOLD);
    Ok(reply)
  }

  async fn destroy(&self, _req: Request) {
    self.neg_cache.clear();
    self.prefetcher.clear().await;
    self.dir_cache.clear(&self.client).await;
  }

  async fn lookup(&self, _req: Request, parent: Inode, name: &OsStr) -> FuseResult<ReplyEntry> {
    self.sync_epoch().await;
    let name = os_str_to_string(name)?;
    if self.neg_cache.contains(parent, &name)
      || self.dir_cache.definitely_missing(parent, &name, self.config.case_insensitive, self.config.entry_ttl).await
    {
      return Err(Errno::new_not_exist());
    }
    if let Some(entry) = self.dir_cache.take_prefetched_lookup(parent, &name, self.config.case_insensitive, self.config.entry_ttl).await
      && let Some(attr) = entry.attr
    {
      return Ok(ReplyEntry {
        ttl: self.config.entry_ttl,
        attr: self.to_file_attr(&attr),
        generation: entry.generation,
      });
    }

    match self.client.lookup(parent, name.clone()).await {
      Ok(entry) => {
        let attr = entry.attr.ok_or_else(|| Errno::from(libc::EIO))?;
        Ok(ReplyEntry {
          ttl: self.config.entry_ttl,
          attr: self.to_file_attr(&attr),
          generation: entry.generation,
        })
      }
      Err(errno) => {
        if errno == libc::ENOENT {
          self.neg_cache.insert(parent, name);
        }
        Err(Errno::from(errno))
      }
    }
  }

  async fn forget(&self, _req: Request, inode: Inode, nlookup: u64) {
    let _ = self
      .client
      .forget(vec![FsForgetItem {
        ino: inode,
        nlookup,
      }])
      .await;
  }

  async fn batch_forget(&self, _req: Request, inodes: &[(Inode, u64)]) {
    let items = inodes.iter().map(|&(ino, nlookup)| FsForgetItem { ino, nlookup }).collect();
    let _ = self.client.forget(items).await;
  }

  async fn getattr(&self, _req: Request, inode: Inode, fh: Option<u64>, _flags: u32) -> FuseResult<ReplyAttr> {
    if let Some(fh) = fh
      && self.write_behind.has_written(fh).await
    {
      self.write_behind.flush(fh).await.map_err(Errno::from)?;
    }
    let attr = self.client.getattr(inode, fh).await.map_err(Errno::from)?;
    Ok(ReplyAttr {
      ttl: self.config.attr_ttl,
      attr: self.to_file_attr(&attr),
    })
  }

  async fn setattr(&self, _req: Request, inode: Inode, fh: Option<u64>, set_attr: SetAttr) -> FuseResult<ReplyAttr> {
    if set_attr.size.is_some() {
      if let Some(fh) = fh {
        self.write_behind.flush(fh).await.map_err(Errno::from)?;
      }
      self.prefetcher.invalidate_inode(inode).await;
    }

    let req = FsSetAttr {
      ino: inode,
      fh,
      mode: set_attr.mode,
      size: set_attr.size,
      atime_sec: set_attr.atime.map(|t| t.sec),
      atime_nsec: set_attr.atime.map(|t| t.nsec),
      mtime_sec: set_attr.mtime.map(|t| t.sec),
      mtime_nsec: set_attr.mtime.map(|t| t.nsec),
    };
    let attr = self.client.setattr(req).await.map_err(Errno::from)?;
    Ok(ReplyAttr {
      ttl: self.config.attr_ttl,
      attr: self.to_file_attr(&attr),
    })
  }

  async fn opendir(&self, _req: Request, inode: Inode, _flags: u32) -> FuseResult<ReplyOpen> {
    Ok(ReplyOpen {
      fh: inode,
      flags: 0,
    })
  }

  async fn readdirplus<'a>(
    &'a self,
    _req: Request,
    parent: Inode,
    _fh: u64,
    offset: u64,
    _lock_owner: u64,
  ) -> FuseResult<ReplyDirectoryPlus<impl Stream<Item = FuseResult<DirectoryEntryPlus>> + Send + 'a>> {
    let entries = self.load_dir_entries(parent, offset).await?;
    let entry_ttl = self.config.entry_ttl;
    let attr_ttl = self.config.attr_ttl;
    let items: Vec<FuseResult<DirectoryEntryPlus>> = entries
      .into_iter()
      .filter(|e| e.offset > offset)
      .filter_map(|e| {
        let raw_attr = e.attr?;
        let attr = self.to_file_attr(&raw_attr);
        Some(Ok(DirectoryEntryPlus {
          inode: e.ino,
          generation: e.generation,
          kind: attr.kind,
          name: OsString::from(e.name),
          offset: e.offset as i64,
          attr,
          entry_ttl,
          attr_ttl,
        }))
      })
      .collect();

    Ok(ReplyDirectoryPlus {
      entries: stream::iter(items),
    })
  }

  async fn releasedir(&self, _req: Request, inode: Inode, _fh: u64, _flags: u32) -> FuseResult<()> {
    self.dir_cache.release_dir(inode).await;
    Ok(())
  }

  async fn open(&self, _req: Request, inode: Inode, flags: u32) -> FuseResult<ReplyOpen> {
    let opened = self.client.open(inode, flags, self.config.writable).await.map_err(Errno::from)?;
    // NOTICE: `ReplyOpen.flags` carries FUSE kernel `FOPEN_*` flags
    // (Reference: Linux `include/uapi/linux/fuse.h`, `FOPEN_DIRECT_IO`..`FOPEN_PASSTHROUGH`),
    // NOT POSIX `O_*` open(2) flags. Forwarding `opened.open_flags` (which holds
    // adjusted POSIX `O_*` bits) misinterprets `O_WRONLY` as `FOPEN_DIRECT_IO`,
    // `O_RDWR` as `FOPEN_KEEP_CACHE`, and `O_EXCL` (0x80) as `FOPEN_PASSTHROUGH`
    // (1 << 7), causing Linux 6.9+ kernels to reject the open with `EIO`.
    Ok(ReplyOpen {
      fh: opened.fh,
      flags: 0,
    })
  }

  async fn read(&self, _req: Request, inode: Inode, fh: u64, offset: u64, size: u32) -> FuseResult<ReplyData> {
    self.sync_epoch().await;
    let data = self.prefetcher.read(inode, fh, offset, size).await.map_err(Errno::from)?;
    Ok(ReplyData { data })
  }

  async fn write(
    &self,
    _req: Request,
    inode: Inode,
    fh: u64,
    offset: u64,
    data: &[u8],
    _write_flags: u32,
    _flags: u32,
  ) -> FuseResult<ReplyWrite> {
    self.prefetcher.invalidate_inode(inode).await;
    let written = self.write_behind.write(inode, fh, offset, data.to_vec()).await.map_err(Errno::from)?;
    Ok(ReplyWrite { written })
  }

  async fn flush(&self, _req: Request, inode: Inode, fh: u64, lock_owner: u64) -> FuseResult<()> {
    let had_writes = self.write_behind.has_written(fh).await;
    self.write_behind.flush(fh).await.map_err(Errno::from)?;
    if self.config.lock_mode != FsLockMode::Remote {
      self.local_locks.unlock_owner(inode, lock_owner);
    }
    // Guard zero-byte `dup` + `close`: only issue a remote flush RPC when data
    // was written or remote POSIX locks must be released for `lock_owner`.
    if had_writes || self.config.lock_mode == FsLockMode::Remote {
      self.client.flush(inode, fh, lock_owner).await.map_err(Errno::from)?;
    }
    Ok(())
  }

  async fn fsync(&self, _req: Request, inode: Inode, fh: u64, datasync: bool) -> FuseResult<()> {
    self.write_behind.flush(fh).await.map_err(Errno::from)?;
    self.client.fsync(inode, fh, datasync).await.map_err(Errno::from)
  }

  async fn release(&self, _req: Request, inode: Inode, fh: u64, _flags: u32, lock_owner: u64, _flush: bool) -> FuseResult<()> {
    let wb_res = self.write_behind.release(fh).await;
    self.prefetcher.release_handle(fh).await;
    if self.config.lock_mode != FsLockMode::Remote {
      self.local_locks.unlock_owner(inode, lock_owner);
    }
    let rel_res = self.client.release(inode, fh, lock_owner).await;
    wb_res.map_err(Errno::from)?;
    rel_res.map_err(Errno::from)
  }

  async fn create(&self, _req: Request, parent: Inode, name: &OsStr, mode: u32, flags: u32) -> FuseResult<ReplyCreated> {
    let name = os_str_to_string(name)?;
    self.neg_cache.invalidate(parent, &name);
    self.invalidate_dir(parent).await;
    let created = self
      .client
      .create(FsCreate {
        parent,
        name,
        mode,
        flags,
        writeback: self.config.writable,
      })
      .await
      .map_err(Errno::from)?;
    let attr = created.attr.ok_or_else(|| Errno::from(libc::EIO))?;
    // NOTICE: `ReplyCreated.flags` expects FUSE `FOPEN_*` flags, not POSIX `O_*`
    // flags from `created.open_flags` (see `open()` above; `O_EXCL` collides with
    // `FOPEN_PASSTHROUGH` and fails `open(O_CREAT | O_EXCL)` with `EIO` on Linux 6.9+).
    Ok(ReplyCreated {
      ttl: self.config.entry_ttl,
      attr: self.to_file_attr(&attr),
      generation: created.generation,
      fh: created.fh,
      flags: 0,
    })
  }

  async fn mkdir(&self, _req: Request, parent: Inode, name: &OsStr, mode: u32, _umask: u32) -> FuseResult<ReplyEntry> {
    let name = os_str_to_string(name)?;
    self.neg_cache.invalidate(parent, &name);
    self.invalidate_dir(parent).await;
    let entry = self.client.mkdir(parent, name, mode).await.map_err(Errno::from)?;
    let attr = entry.attr.ok_or_else(|| Errno::from(libc::EIO))?;
    Ok(ReplyEntry {
      ttl: self.config.entry_ttl,
      attr: self.to_file_attr(&attr),
      generation: entry.generation,
    })
  }

  async fn unlink(&self, _req: Request, parent: Inode, name: &OsStr) -> FuseResult<()> {
    let name = os_str_to_string(name)?;
    self.neg_cache.invalidate(parent, &name);
    self.invalidate_dir(parent).await;
    self.client.unlink(parent, name).await.map_err(Errno::from)
  }

  async fn rmdir(&self, _req: Request, parent: Inode, name: &OsStr) -> FuseResult<()> {
    let name = os_str_to_string(name)?;
    self.neg_cache.invalidate(parent, &name);
    self.invalidate_dir(parent).await;
    self.client.rmdir(parent, name).await.map_err(Errno::from)
  }

  async fn rename(&self, req: Request, parent: Inode, name: &OsStr, new_parent: Inode, new_name: &OsStr) -> FuseResult<()> {
    self.rename2(req, parent, name, new_parent, new_name, 0).await
  }

  async fn rename2(&self, _req: Request, parent: Inode, name: &OsStr, new_parent: Inode, new_name: &OsStr, flags: u32) -> FuseResult<()> {
    let name = os_str_to_string(name)?;
    let new_name = os_str_to_string(new_name)?;
    self.neg_cache.invalidate(parent, &name);
    self.neg_cache.invalidate(new_parent, &new_name);
    self.invalidate_dir(parent).await;
    if new_parent != parent {
      self.invalidate_dir(new_parent).await;
    }
    self
      .client
      .rename(FsRename {
        parent,
        name,
        new_parent,
        new_name,
        flags,
      })
      .await
      .map_err(Errno::from)
  }

  async fn symlink(&self, _req: Request, parent: Inode, name: &OsStr, link: &OsStr) -> FuseResult<ReplyEntry> {
    let name = os_str_to_string(name)?;
    let link_target = os_str_to_string(link)?;
    self.neg_cache.invalidate(parent, &name);
    self.invalidate_dir(parent).await;
    let entry = self.client.symlink(parent, name, link_target).await.map_err(Errno::from)?;
    let attr = entry.attr.ok_or_else(|| Errno::from(libc::EIO))?;
    Ok(ReplyEntry {
      ttl: self.config.entry_ttl,
      attr: self.to_file_attr(&attr),
      generation: entry.generation,
    })
  }

  async fn readlink(&self, _req: Request, inode: Inode) -> FuseResult<ReplyData> {
    let target = self.client.readlink(inode).await.map_err(Errno::from)?;
    Ok(ReplyData {
      data: Bytes::from(target.into_bytes()),
    })
  }

  async fn link(&self, _req: Request, inode: Inode, new_parent: Inode, new_name: &OsStr) -> FuseResult<ReplyEntry> {
    let new_name = os_str_to_string(new_name)?;
    self.neg_cache.invalidate(new_parent, &new_name);
    self.invalidate_dir(new_parent).await;
    let entry = self.client.link(inode, new_parent, new_name).await.map_err(Errno::from)?;
    let attr = entry.attr.ok_or_else(|| Errno::from(libc::EIO))?;
    Ok(ReplyEntry {
      ttl: self.config.entry_ttl,
      attr: self.to_file_attr(&attr),
      generation: entry.generation,
    })
  }

  async fn statfs(&self, _req: Request, inode: Inode) -> FuseResult<ReplyStatFs> {
    let st = self.client.statfs(inode).await.map_err(Errno::from)?;
    Ok(ReplyStatFs {
      blocks: st.blocks,
      bfree: st.bfree,
      bavail: st.bavail,
      files: st.files,
      ffree: st.ffree,
      bsize: st.bsize,
      namelen: st.namelen,
      frsize: st.frsize,
    })
  }

  async fn getlk(
    &self,
    _req: Request,
    inode: Inode,
    fh: u64,
    lock_owner: u64,
    start: u64,
    end: u64,
    r#type: u32,
    pid: u32,
  ) -> FuseResult<ReplyLock> {
    if self.config.lock_mode != FsLockMode::Remote {
      return Ok(self.local_locks.getlk(inode, lock_owner, start, end, r#type));
    }
    let range = self
      .client
      .getlock(FsGetLock {
        ino: inode,
        fh,
        lock_owner,
        range: Some(FsLockRange {
          start,
          end,
          kind: posix_lock_type_to_proto(r#type).into(),
          pid,
        }),
      })
      .await
      .map_err(Errno::from)?;
    Ok(ReplyLock {
      start: range.start,
      end: range.end,
      r#type: proto_lock_type_to_posix(range.kind),
      pid: range.pid,
    })
  }

  async fn setlk(
    &self,
    _req: Request,
    inode: Inode,
    fh: u64,
    lock_owner: u64,
    start: u64,
    end: u64,
    r#type: u32,
    pid: u32,
    block: bool,
  ) -> FuseResult<()> {
    if self.config.lock_mode != FsLockMode::Remote {
      return self
        .local_locks
        .setlk(
          inode,
          LocalLock {
            owner: lock_owner,
            start,
            end,
            kind: r#type,
            pid,
          },
          block,
        )
        .await;
    }
    self
      .client
      .setlock(FsSetLock {
        ino: inode,
        fh,
        lock_owner,
        range: Some(FsLockRange {
          start,
          end,
          kind: posix_lock_type_to_proto(r#type).into(),
          pid,
        }),
        sleep: block,
      })
      .await
      .map_err(Errno::from)
  }
}

/// Handle to a running FUSE3 mount.
pub struct MountGuard {
  target: String,
  cancel: CancellationToken,
  task: JoinHandle<()>,
}

impl MountGuard {
  /// Returns the mount target path.
  pub fn target(&self) -> &str {
    &self.target
  }

  /// Triggers an orderly unmount and waits for the FUSE session task to exit.
  pub async fn unmount(self) -> Result<()> {
    self.cancel.cancel();
    let _ = self.task.await;
    Ok(())
  }

  /// Waits for the FUSE session task to exit (either because the mount was
  /// detached externally or because the underlying stream was cancelled).
  pub async fn wait(self) {
    let _ = self.task.await;
  }
}

fn build_mount_options(config: &MountConfig, allow_other: bool) -> MountOptions {
  let mut opts = MountOptions::default();
  opts
    .fs_name("needy-fs")
    .write_back(config.writable)
    .force_readdir_plus(true)
    .default_permissions(true)
    .allow_other(allow_other)
    .uid(config.uid)
    .gid(config.gid)
    .read_only(!config.writable);
  opts
}

/// Mounts a workspace over an existing [`FsRpcClient`], performing the initial
/// `FsMount` handshake and monitoring `client.cancelled()` for automatic
/// unmount on disconnect or `FsForceUnmount`.
pub async fn spawn_mount(client: Arc<FsRpcClient>, mut config: MountConfig) -> Result<MountGuard> {
  crate::sshfs::create_mount_target(Path::new(&config.target), config.uid, config.gid)?;

  let mounted = client
    .mount(FsMount {
      executor: config.executor.clone(),
      source: config.source.clone(),
      target: config.target.clone(),
      writable: config.writable,
      lock_mode: config.lock_mode.into(),
    })
    .await
    .map_err(|errno| anyhow::anyhow!("FsMount failed with errno {errno}"))?;

  config.writable = mounted.writable;
  config.case_insensitive = mounted.case_insensitive;
  if let Ok(mode) = FsLockMode::try_from(mounted.lock_mode)
    && mode != FsLockMode::Unspecified
  {
    config.lock_mode = mode;
  }

  let target = config.target.clone();
  let is_root = nix::unistd::geteuid().is_root();
  let mount_handle = if is_root {
    let fs = NeedyFs::new(Arc::clone(&client), config.clone());
    let opts = build_mount_options(&config, true);
    Session::new(opts).mount(fs, &target).await.with_context(|| format!("failed to mount FUSE3 at {target}"))?
  } else {
    let fs = NeedyFs::new(Arc::clone(&client), config.clone());
    let opts = build_mount_options(&config, true);
    match Session::new(opts).mount_with_unprivileged(fs, &target).await {
      Ok(handle) => handle,
      Err(err) => {
        tracing::warn!("unprivileged FUSE3 mount with allow_other failed at {target} ({err}); retrying without allow_other");
        let fs = NeedyFs::new(Arc::clone(&client), config.clone());
        let opts = build_mount_options(&config, false);
        Session::new(opts)
          .mount_with_unprivileged(fs, &target)
          .await
          .with_context(|| format!("failed to mount unprivileged FUSE3 at {target}"))?
      }
    }
  };

  let cancel = client.cancelled();
  let guard_cancel = cancel.clone();
  let target_for_task = target.clone();
  let task = tokio::spawn(async move {
    let mut handle = mount_handle;
    tokio::select! {
      res = &mut handle => {
        if let Err(err) = res {
          tracing::warn!("FUSE3 session at {target_for_task} exited with error: {err}");
        } else {
          tracing::debug!("FUSE3 session at {target_for_task} exited");
        }
      }
      _ = cancel.cancelled() => {
        tracing::info!("unmounting FUSE3 at {target_for_task} after session cancellation");
        match tokio::time::timeout(Duration::from_secs(5), handle.unmount()).await {
          Ok(Ok(())) => {}
          Ok(Err(err)) => {
            tracing::warn!("FUSE3 unmount at {target_for_task} failed ({err}); detaching");
            let _ = detach(&target_for_task);
          }
          Err(_) => {
            tracing::warn!("FUSE3 unmount at {target_for_task} timed out; detaching");
            let _ = detach(&target_for_task);
          }
        }
      }
    }
  });

  Ok(MountGuard {
    target,
    cancel: guard_cancel,
    task,
  })
}

/// Convenience entry point that constructs an [`FsRpcClient`] from a raw
/// bidirectional `FsService::Session` stream and mounts it at `config.target`.
pub async fn mount_with_stream<S>(tx: mpsc::Sender<SessionRequest>, rx: S, config: MountConfig) -> Result<MountGuard>
where
  S: futures_util::Stream<Item = std::result::Result<SessionResponse, tonic::Status>> + Send + 'static,
{
  let client = FsRpcClient::from_stream(tx, rx);
  spawn_mount(client, config).await
}

#[cfg(test)]
mod tests {
  use super::*;
  use needy_api_proto::v1::{FsCreated, FsOpened, session_request, session_response};
  use tokio_stream::wrappers::ReceiverStream;

  // ROOT CAUSE:
  //
  // If a guest opened or created a file with POSIX `O_*` flags (such as
  // `O_WRONLY | O_CREAT | O_EXCL` used for atomic temp-file writes), the
  // executor returned adjusted POSIX `O_*` bits in `FsOpened.open_flags` /
  // `FsCreated.open_flags` and `NeedyFs` forwarded them verbatim into FUSE's
  // `ReplyOpen.flags` / `ReplyCreated.flags` (`FOPEN_*` bitmask). On Linux 6.9+
  // kernels, `O_EXCL` (0x80) collided with `FOPEN_PASSTHROUGH` (1 << 7) without
  // a backing_id and caused the kernel to fail the syscall with `EIO`, leaving
  // behind a 0-byte file on the executor.
  //
  // Before the fix, `ReplyOpen.flags` and `ReplyCreated.flags` carried POSIX `O_*` bits.
  // The fix returns `0` (valid `FOPEN_*` flags) for both `open` and `create`.
  #[tokio::test]
  async fn open_and_create_do_not_forward_posix_o_flags_as_fopen_flags() {
    let (req_tx, mut req_rx) = mpsc::channel::<SessionRequest>(8);
    let (resp_tx, resp_rx) = mpsc::channel::<std::result::Result<SessionResponse, tonic::Status>>(8);

    let server = tokio::spawn(async move {
      let posix_flags = (libc::O_RDWR | libc::O_CREAT | libc::O_EXCL) as u32;
      while let Some(req) = req_rx.recv().await {
        let body = match req.body {
          Some(session_request::Body::Create(_)) => session_response::Body::Created(FsCreated {
            attr: Some(FsAttr {
              ino: 2,
              size: 0,
              blocks: 0,
              atime_sec: 0,
              atime_nsec: 0,
              mtime_sec: 0,
              mtime_nsec: 0,
              ctime_sec: 0,
              ctime_nsec: 0,
              kind: FsFileType::Regular as i32,
              perm: 0o600,
              nlink: 1,
              uid: 1000,
              gid: 1000,
              rdev: 0,
              blksize: 4096,
            }),
            generation: 1,
            fh: 11,
            open_flags: posix_flags,
          }),
          Some(session_request::Body::Open(_)) => session_response::Body::Opened(FsOpened {
            fh: 12,
            open_flags: posix_flags,
          }),
          _ => continue,
        };
        let _ = resp_tx
          .send(Ok(SessionResponse {
            request_id: req.request_id,
            body: Some(body),
          }))
          .await;
      }
    });

    let client = FsRpcClient::from_stream(req_tx, ReceiverStream::new(resp_rx));
    let config = MountConfig::new("/tmp/mnt", "build", "/src", true, 1000, 1000);
    let fs = NeedyFs::new(client, config);
    let req = Request {
      unique: 1,
      uid: 1000,
      gid: 1000,
      pid: 1,
    };

    let created = fs
      .create(req, 1, OsStr::new(".credentials.json.tmp"), 0o600, (libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL) as u32)
      .await
      .expect("create should succeed");
    assert_eq!(created.fh, 11);
    assert_eq!(created.flags, 0);

    let opened = fs.open(req, 2, (libc::O_RDWR | libc::O_EXCL) as u32).await.expect("open should succeed");
    assert_eq!(opened.fh, 12);
    assert_eq!(opened.flags, 0);

    drop(fs);
    let _ = server.await;
  }
}
