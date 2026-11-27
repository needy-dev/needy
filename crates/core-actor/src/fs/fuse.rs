//! Linux FUSE3 `raw::Filesystem` implementation backed by [`FsRpcClient`].

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
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
use tokio::sync::{Mutex, Notify, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::client::FsRpcClient;
use super::neg_cache::{DEFAULT_CAPACITY, DEFAULT_TTL, NegativeCache};
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

struct DirCacheEntry {
  entries: Vec<FsDirEntry>,
  fetched_at: Instant,
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
  dir_cache: Mutex<HashMap<u64, DirCacheEntry>>,
  local_locks: LocalLockTable,
}

impl NeedyFs {
  /// Constructs a [`NeedyFs`] instance from an already-mounted [`FsRpcClient`].
  pub fn new(client: Arc<FsRpcClient>, config: MountConfig) -> Self {
    let last_epoch = AtomicU64::new(client.session_epoch());
    let prefetcher = Prefetcher::new(Arc::clone(&client));
    let write_behind = WriteBehind::new(Arc::clone(&client));
    let neg_cache = NegativeCache::with_case_sensitivity(DEFAULT_CAPACITY, DEFAULT_TTL, config.case_insensitive);
    Self {
      client,
      config,
      last_epoch,
      neg_cache,
      prefetcher,
      write_behind,
      dir_cache: Mutex::new(HashMap::new()),
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
      self.dir_cache.lock().await.clear();
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
    let mut cache = self.dir_cache.lock().await;
    cache.remove(&parent);
  }

  async fn load_dir_entries(&self, parent: u64) -> FuseResult<Vec<FsDirEntry>> {
    self.sync_epoch().await;
    {
      let cache = self.dir_cache.lock().await;
      if let Some(entry) = cache.get(&parent)
        && entry.fetched_at.elapsed() < self.config.entry_ttl
      {
        return Ok(entry.entries.clone());
      }
    }

    let mut all_entries = Vec::new();
    let mut offset = 0u64;
    loop {
      let reply = self.client.readdirplus(parent, offset, READDIRPLUS_BATCH_SIZE).await.map_err(Errno::from)?;
      let last_offset = reply.entries.last().map(|e| e.offset);
      all_entries.extend(reply.entries);
      if reply.eof {
        break;
      }
      match last_offset {
        Some(next) if next > offset => offset = next,
        _ => break,
      }
    }

    let mut cache = self.dir_cache.lock().await;
    cache.insert(
      parent,
      DirCacheEntry {
        entries: all_entries.clone(),
        fetched_at: Instant::now(),
      },
    );
    Ok(all_entries)
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
    let mut reply = ReplyInit::new(NonZeroU32::new(MAX_WRITE_BYTES).expect("non-zero"));
    reply.max_background = Some(MAX_BACKGROUND_REQUESTS);
    reply.congestion_threshold = Some(CONGESTION_THRESHOLD);
    Ok(reply)
  }

  async fn destroy(&self, _req: Request) {
    self.neg_cache.clear();
    self.prefetcher.clear().await;
    self.dir_cache.lock().await.clear();
  }

  async fn lookup(&self, _req: Request, parent: Inode, name: &OsStr) -> FuseResult<ReplyEntry> {
    self.sync_epoch().await;
    let name = os_str_to_string(name)?;
    if self.neg_cache.contains(parent, &name) {
      return Err(Errno::new_not_exist());
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
    let entries = self.load_dir_entries(parent).await?;
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
    self.invalidate_dir(inode).await;
    Ok(())
  }

  async fn open(&self, _req: Request, inode: Inode, flags: u32) -> FuseResult<ReplyOpen> {
    let opened = self.client.open(inode, flags, self.config.writable).await.map_err(Errno::from)?;
    Ok(ReplyOpen {
      fh: opened.fh,
      flags: opened.open_flags,
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
    Ok(ReplyCreated {
      ttl: self.config.entry_ttl,
      attr: self.to_file_attr(&attr),
      generation: created.generation,
      fh: created.fh,
      flags: created.open_flags,
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
