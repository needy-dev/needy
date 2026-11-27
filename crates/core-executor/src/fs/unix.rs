//! macOS / BSD executor filesystem server (`UnixFs`):
//! - `openat` + `O_NOFOLLOW | O_CLOEXEC` + `fstatat` with a `(dev, ino)` inode
//!   table and non-zero CAS refcounting (`Forget` eviction).
//! - Regular files and symlinks never pin FDs in `InodeData` (only active open
//!   `HandleData` entries hold regular-file FDs); non-root directory FDs are
//!   bounded by an LRU cache (`DIR_FD_CACHE_CAP`) and reopened transparently on
//!   miss by walking `loc` up to the nearest cached ancestor (at worst
//!   `ROOT_INO`, which stays permanently pinned).
//! - `is_safe_inode` gate allowing only regular files, directories and symlinks.
//! - Hybrid in-process `AdvisoryLockTable` + `fcntl(F_GETLK/F_SETLK)` so
//!   multi-handle lock conflicts within the same executor process AND across
//!   external host processes are both detected accurately.

use std::collections::HashMap;
use std::ffi::{CStr, CString, OsStr};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use needy_api_proto::v1;

use super::{AdvisoryLockTable, FileName, ROOT_INO, adjust_writeback_flags, errno, fs_err, io_to_fs_err, linux_flags};

type DevIno = (u64, u64);

/// Default maximum number of non-root directory FDs kept open in `UnixFs`.
const DIR_FD_CACHE_CAP: usize = 1024;

struct InodeData {
  dev_ino: DevIno,
  kind: v1::FsFileType,
  /// Last known `(parent_ino, name)` for reopening evicted directories,
  /// statting/opening closed regular files, and reading symlinks via `openat`.
  loc: Mutex<Option<(u64, CString)>>,
  refcount: AtomicU64,
}

impl InodeData {
  fn try_inc_ref(&self) -> bool {
    let mut cur = self.refcount.load(Ordering::Acquire);
    while cur > 0 {
      let Some(next) = cur.checked_add(1) else {
        return false;
      };
      match self.refcount.compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => return true,
        Err(actual) => cur = actual,
      }
    }
    false
  }

  fn dec_ref(&self, n: u64) -> u64 {
    let mut cur = self.refcount.load(Ordering::Acquire);
    loop {
      let next = cur.saturating_sub(n);
      match self.refcount.compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire) {
        Ok(prev) => return prev,
        Err(actual) => cur = actual,
      }
    }
  }
}

struct InodeStore {
  by_ino: HashMap<u64, Arc<InodeData>>,
  by_key: HashMap<DevIno, u64>,
}

/// Bounded LRU cache of non-root directory file descriptors.
///
/// Cache hits and removals are O(1); eviction only scans the cache when
/// inserting above `cap` (default 1024).
struct DirFdCache {
  cap: usize,
  tick: u64,
  entries: HashMap<u64, (Arc<OwnedFd>, u64)>,
}

impl DirFdCache {
  fn new(cap: usize) -> Self {
    Self {
      cap,
      tick: 0,
      entries: HashMap::new(),
    }
  }

  fn next_tick(&mut self) -> u64 {
    self.tick = self.tick.wrapping_add(1);
    self.tick
  }

  fn get(&mut self, ino: u64) -> Option<Arc<OwnedFd>> {
    let tick = self.next_tick();
    let entry = self.entries.get_mut(&ino)?;
    entry.1 = tick;
    Some(Arc::clone(&entry.0))
  }

  fn insert(&mut self, ino: u64, fd: Arc<OwnedFd>) {
    if ino == ROOT_INO || self.cap == 0 {
      return;
    }
    let tick = self.next_tick();
    self.entries.insert(ino, (fd, tick));
    while self.entries.len() > self.cap {
      if let Some(&lru_ino) = self.entries.iter().min_by_key(|&(_, &(_, t))| t).map(|(k, _)| k) {
        self.entries.remove(&lru_ino);
      } else {
        break;
      }
    }
  }

  fn remove(&mut self, ino: u64) {
    self.entries.remove(&ino);
  }

  #[cfg(test)]
  fn len(&self) -> usize {
    self.entries.len()
  }
}

struct HandleData {
  ino: u64,
  writable: bool,
  fd: OwnedFd,
}

pub(crate) struct UnixFs {
  root_attr: v1::FsAttr,
  /// Permanently pinned workspace root FD (never evicted).
  root_fd: Arc<OwnedFd>,
  case_insensitive: bool,
  inodes: RwLock<InodeStore>,
  dir_fds: Mutex<DirFdCache>,
  handles: RwLock<HashMap<u64, Arc<HandleData>>>,
  locks: AdvisoryLockTable,
  next_ino: AtomicU64,
  next_fh: AtomicU64,
}

impl UnixFs {
  pub(crate) fn open(root: &Path) -> Result<Self, v1::FsError> {
    Self::open_with_dir_cache_cap(root, DIR_FD_CACHE_CAP)
  }

  fn open_with_dir_cache_cap(root: &Path, dir_fd_cache_cap: usize) -> Result<Self, v1::FsError> {
    let c_root = path_to_cstring(root)?;
    let raw = unsafe { libc::open(c_root.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
    if raw < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    let fd = Arc::new(unsafe { OwnedFd::from_raw_fd(raw) });
    let st = fstat_fd(fd.as_raw_fd())?;
    if !is_safe_inode(st.st_mode) || (st.st_mode & libc::S_IFMT) != libc::S_IFDIR {
      return Err(fs_err(errno::ENOTDIR, "workspace root is not a directory"));
    }

    let case_insensitive = detect_case_insensitive(fd.as_raw_fd());
    let dev_ino = stat_dev_ino(&st);
    let root_attr = stat_to_attr(ROOT_INO, &st);
    let root_data = Arc::new(InodeData {
      dev_ino,
      kind: v1::FsFileType::Directory,
      loc: Mutex::new(None),
      refcount: AtomicU64::new(1),
    });

    let mut by_ino = HashMap::new();
    let mut by_key = HashMap::new();
    by_ino.insert(ROOT_INO, root_data);
    by_key.insert(dev_ino, ROOT_INO);

    Ok(Self {
      root_attr,
      root_fd: fd,
      case_insensitive,
      inodes: RwLock::new(InodeStore { by_ino, by_key }),
      dir_fds: Mutex::new(DirFdCache::new(dir_fd_cache_cap)),
      handles: RwLock::new(HashMap::new()),
      locks: AdvisoryLockTable::new(),
      next_ino: AtomicU64::new(ROOT_INO + 1),
      next_fh: AtomicU64::new(1),
    })
  }

  #[cfg(test)]
  pub(crate) fn pinned_fd_count(&self) -> usize {
    // 1 for the permanently pinned `root_fd`, plus cached non-root directory FDs.
    // Regular files and symlinks never hold pinned FDs in `InodeData`.
    1 + self.dir_fds.lock().unwrap_or_else(|e| e.into_inner()).len()
  }

  pub(crate) fn root_attr(&self) -> v1::FsAttr {
    self.root_attr
  }

  pub(crate) fn case_insensitive(&self) -> bool {
    self.case_insensitive
  }

  fn get_inode(&self, ino: u64) -> Result<Arc<InodeData>, v1::FsError> {
    let store = self.inodes.read().unwrap_or_else(|e| e.into_inner());
    store.by_ino.get(&ino).cloned().ok_or_else(|| fs_err(errno::ENOENT, format!("unknown inode {ino}")))
  }

  fn get_cached_dir_fd(&self, ino: u64) -> Option<Arc<OwnedFd>> {
    self.dir_fds.lock().unwrap_or_else(|e| e.into_inner()).get(ino)
  }

  fn cache_dir_fd(&self, ino: u64, fd: Arc<OwnedFd>) {
    self.dir_fds.lock().unwrap_or_else(|e| e.into_inner()).insert(ino, fd);
  }

  fn evict_dir_fd(&self, ino: u64) {
    self.dir_fds.lock().unwrap_or_else(|e| e.into_inner()).remove(ino);
  }

  /// Return an open directory FD for `ino`, reopening evicted non-root
  /// directories transparently by walking `loc` up to the nearest cached
  /// ancestor (at worst `ROOT_INO`).
  fn ensure_dir_fd(&self, ino: u64) -> Result<Arc<OwnedFd>, v1::FsError> {
    if ino == ROOT_INO {
      return Ok(Arc::clone(&self.root_fd));
    }
    if let Some(fd) = self.get_cached_dir_fd(ino) {
      return Ok(fd);
    }

    let mut chain: Vec<(u64, CString)> = Vec::new();
    let mut cur = ino;
    let mut anchor_fd: Option<Arc<OwnedFd>> = None;
    let mut depth = 0usize;
    while cur != ROOT_INO {
      if let Some(fd) = self.get_cached_dir_fd(cur) {
        anchor_fd = Some(fd);
        break;
      }
      depth += 1;
      if depth > 4096 {
        return Err(fs_err(errno::ELOOP, "directory ancestor chain too deep"));
      }
      let node = self.get_inode(cur)?;
      if node.kind != v1::FsFileType::Directory {
        return Err(fs_err(errno::ENOTDIR, "not a directory"));
      }
      let (parent, name) = node_loc(&node)?;
      chain.push((cur, name));
      cur = parent;
    }

    let mut cur_fd = anchor_fd.unwrap_or_else(|| Arc::clone(&self.root_fd));
    for (dir_ino, name) in chain.into_iter().rev() {
      let raw =
        unsafe { libc::openat(cur_fd.as_raw_fd(), name.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
      if raw < 0 {
        return Err(io_to_fs_err(io::Error::last_os_error()));
      }
      let owned = Arc::new(unsafe { OwnedFd::from_raw_fd(raw) });
      self.cache_dir_fd(dir_ino, Arc::clone(&owned));
      cur_fd = owned;
    }
    Ok(cur_fd)
  }

  fn get_handle(&self, fh: u64) -> Result<Arc<HandleData>, v1::FsError> {
    let handles = self.handles.read().unwrap_or_else(|e| e.into_inner());
    handles.get(&fh).cloned().ok_or_else(|| fs_err(errno::EBADF, format!("unknown file handle {fh}")))
  }

  fn find_open_handle_for_ino(&self, ino: u64) -> Option<Arc<HandleData>> {
    let handles = self.handles.read().unwrap_or_else(|e| e.into_inner());
    handles.values().find(|h| h.ino == ino).cloned()
  }

  fn find_writable_open_handle_for_ino(&self, ino: u64) -> Option<Arc<HandleData>> {
    let handles = self.handles.read().unwrap_or_else(|e| e.into_inner());
    handles.values().find(|h| h.ino == ino && h.writable).cloned()
  }

  fn insert_handle(&self, ino: u64, fd: OwnedFd, writable: bool) -> u64 {
    let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
    let mut handles = self.handles.write().unwrap_or_else(|e| e.into_inner());
    handles.insert(fh, Arc::new(HandleData { ino, writable, fd }));
    fh
  }

  fn register_inode(&self, parent: u64, c_name: CString, st: &libc::stat) -> Result<v1::FsAttr, v1::FsError> {
    if !is_safe_inode(st.st_mode) {
      return Err(fs_err(errno::EACCES, "unsupported special file type"));
    }
    let kind = mode_to_file_type(st.st_mode);
    let dev_ino = stat_dev_ino(st);

    {
      let store = self.inodes.read().unwrap_or_else(|e| e.into_inner());
      if let Some(&ino) = store.by_key.get(&dev_ino)
        && let Some(existing) = store.by_ino.get(&ino)
        && existing.try_inc_ref()
      {
        *existing.loc.lock().unwrap_or_else(|e| e.into_inner()) = Some((parent, c_name));
        return Ok(stat_to_attr(ino, st));
      }
    }

    let mut store = self.inodes.write().unwrap_or_else(|e| e.into_inner());
    if let Some(&ino) = store.by_key.get(&dev_ino)
      && let Some(existing) = store.by_ino.get(&ino)
      && existing.try_inc_ref()
    {
      *existing.loc.lock().unwrap_or_else(|e| e.into_inner()) = Some((parent, c_name));
      return Ok(stat_to_attr(ino, st));
    }

    let ino = self.next_ino.fetch_add(1, Ordering::Relaxed);
    let data = Arc::new(InodeData {
      dev_ino,
      kind,
      loc: Mutex::new(Some((parent, c_name))),
      refcount: AtomicU64::new(1),
    });
    store.by_ino.insert(ino, data);
    store.by_key.insert(dev_ino, ino);
    Ok(stat_to_attr(ino, st))
  }

  pub(crate) fn lookup(&self, parent: u64, name: FileName<'_>) -> Result<v1::FsAttr, v1::FsError> {
    let dir_fd = self.ensure_dir_fd(parent)?;
    let c_name = name_to_cstring(name)?;
    let st = fstatat_nofollow(dir_fd.as_raw_fd(), &c_name)?;
    if !is_safe_inode(st.st_mode) {
      return Err(fs_err(errno::EACCES, "unsupported special file type"));
    }

    let child_dir_fd = if (st.st_mode & libc::S_IFMT) == libc::S_IFDIR {
      let raw = unsafe {
        libc::openat(dir_fd.as_raw_fd(), c_name.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
      };
      if raw < 0 {
        return Err(io_to_fs_err(io::Error::last_os_error()));
      }
      Some(Arc::new(unsafe { OwnedFd::from_raw_fd(raw) }))
    } else {
      None
    };

    let attr = self.register_inode(parent, c_name, &st)?;
    if let Some(fd) = child_dir_fd {
      self.cache_dir_fd(attr.ino, fd);
    }
    Ok(attr)
  }

  pub(crate) fn getattr(&self, ino: u64, fh: Option<u64>) -> Result<v1::FsAttr, v1::FsError> {
    if let Some(fh) = fh {
      let handle = self.get_handle(fh)?;
      let st = fstat_fd(handle.fd.as_raw_fd())?;
      return Ok(stat_to_attr(ino, &st));
    }
    let node = self.get_inode(ino)?;
    if node.kind == v1::FsFileType::Directory {
      let dir_fd = self.ensure_dir_fd(ino)?;
      let st = fstat_fd(dir_fd.as_raw_fd())?;
      return Ok(stat_to_attr(ino, &st));
    }
    // Fall back to an active open handle for `ino` so open-unlinked files (and
    // files replaced by atomic rename while open) still answer `getattr(ino, None)`.
    if let Some(handle) = self.find_open_handle_for_ino(ino) {
      let st = fstat_fd(handle.fd.as_raw_fd())?;
      return Ok(stat_to_attr(ino, &st));
    }
    let (parent, name) = node_loc(&node)?;
    let pfd = self.ensure_dir_fd(parent)?;
    let st = fstatat_nofollow(pfd.as_raw_fd(), &name)?;
    Ok(stat_to_attr(ino, &st))
  }

  pub(crate) fn setattr(&self, req: &v1::FsSetAttr) -> Result<v1::FsAttr, v1::FsError> {
    let handle = match req.fh {
      Some(fh) => Some(self.get_handle(fh)?),
      None => None,
    };
    let node = self.get_inode(req.ino)?;

    if let Some(mode) = req.mode {
      let rc = if let Some(ref h) = handle {
        unsafe { libc::fchmod(h.fd.as_raw_fd(), (mode & 0o7777) as libc::mode_t) }
      } else if node.kind == v1::FsFileType::Directory {
        let dir_fd = self.ensure_dir_fd(req.ino)?;
        unsafe { libc::fchmod(dir_fd.as_raw_fd(), (mode & 0o7777) as libc::mode_t) }
      } else if let Some(h) = self.find_open_handle_for_ino(req.ino) {
        unsafe { libc::fchmod(h.fd.as_raw_fd(), (mode & 0o7777) as libc::mode_t) }
      } else {
        let (parent, name) = node_loc(&node)?;
        let pfd = self.ensure_dir_fd(parent)?;
        unsafe { libc::fchmodat(pfd.as_raw_fd(), name.as_ptr(), (mode & 0o7777) as libc::mode_t, 0) }
      };
      if rc < 0 {
        return Err(io_to_fs_err(io::Error::last_os_error()));
      }
    }

    if let Some(size) = req.size {
      let rc = if let Some(ref h) = handle {
        unsafe { libc::ftruncate(h.fd.as_raw_fd(), size as libc::off_t) }
      } else if let Some(h) = self.find_writable_open_handle_for_ino(req.ino) {
        unsafe { libc::ftruncate(h.fd.as_raw_fd(), size as libc::off_t) }
      } else {
        let (parent, name) = node_loc(&node)?;
        let pfd = self.ensure_dir_fd(parent)?;
        let raw = unsafe { libc::openat(pfd.as_raw_fd(), name.as_ptr(), libc::O_WRONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
        if raw < 0 {
          return Err(io_to_fs_err(io::Error::last_os_error()));
        }
        let tmp = unsafe { OwnedFd::from_raw_fd(raw) };
        unsafe { libc::ftruncate(tmp.as_raw_fd(), size as libc::off_t) }
      };
      if rc < 0 {
        return Err(io_to_fs_err(io::Error::last_os_error()));
      }
    }

    if req.atime_sec.is_some() || req.mtime_sec.is_some() {
      let times = [
        match req.atime_sec {
          Some(sec) => libc::timespec {
            tv_sec: sec as libc::time_t,
            tv_nsec: req.atime_nsec.unwrap_or(0) as libc::c_long,
          },
          None => libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_OMIT,
          },
        },
        match req.mtime_sec {
          Some(sec) => libc::timespec {
            tv_sec: sec as libc::time_t,
            tv_nsec: req.mtime_nsec.unwrap_or(0) as libc::c_long,
          },
          None => libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_OMIT,
          },
        },
      ];
      let rc = if let Some(ref h) = handle {
        unsafe { libc::futimens(h.fd.as_raw_fd(), times.as_ptr()) }
      } else if node.kind == v1::FsFileType::Directory {
        let dir_fd = self.ensure_dir_fd(req.ino)?;
        unsafe { libc::futimens(dir_fd.as_raw_fd(), times.as_ptr()) }
      } else if let Some(h) = self.find_open_handle_for_ino(req.ino) {
        unsafe { libc::futimens(h.fd.as_raw_fd(), times.as_ptr()) }
      } else {
        let (parent, name) = node_loc(&node)?;
        let pfd = self.ensure_dir_fd(parent)?;
        unsafe { libc::utimensat(pfd.as_raw_fd(), name.as_ptr(), times.as_ptr(), libc::AT_SYMLINK_NOFOLLOW) }
      };
      if rc < 0 {
        return Err(io_to_fs_err(io::Error::last_os_error()));
      }
    }

    self.getattr(req.ino, req.fh)
  }

  pub(crate) fn readdirplus(
    &self,
    ino: u64,
    offset: u64,
    max_entries: usize,
    generation: u64,
  ) -> Result<(Vec<v1::FsDirEntry>, bool), v1::FsError> {
    let dir_fd = self.ensure_dir_fd(ino)?;
    let dup_fd = unsafe { libc::fcntl(dir_fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if dup_fd < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    let dir_ptr = unsafe { libc::fdopendir(dup_fd) };
    if dir_ptr.is_null() {
      let err = io::Error::last_os_error();
      unsafe { libc::close(dup_fd) };
      return Err(io_to_fs_err(err));
    }

    struct DirGuard(*mut libc::DIR);
    impl Drop for DirGuard {
      fn drop(&mut self) {
        unsafe { libc::closedir(self.0) };
      }
    }
    let guard = DirGuard(dir_ptr);
    unsafe { libc::rewinddir(guard.0) };

    let mut names: Vec<String> = Vec::new();
    loop {
      let ent = unsafe { libc::readdir(guard.0) };
      if ent.is_null() {
        break;
      }
      let c_name = unsafe { CStr::from_ptr((*ent).d_name.as_ptr()) };
      let Ok(s) = c_name.to_str() else { continue };
      if s == "." || s == ".." {
        continue;
      }
      names.push(s.to_owned());
    }
    drop(guard);

    names.sort();

    let start = offset as usize;
    let mut entries = Vec::new();
    let mut idx = start;
    while idx < names.len() && entries.len() < max_entries {
      let name = &names[idx];
      idx += 1;
      let Ok(valid) = FileName::parse(name) else {
        continue;
      };
      match self.lookup(ino, valid) {
        Ok(attr) => {
          entries.push(v1::FsDirEntry {
            ino: attr.ino,
            offset: idx as u64,
            name: name.clone(),
            attr: Some(attr),
            generation,
          });
        }
        Err(e) if e.errno == errno::ENOENT || e.errno == errno::EACCES => continue,
        Err(e) => return Err(e),
      }
    }
    let eof = idx >= names.len();
    Ok((entries, eof))
  }

  pub(crate) fn open_file(&self, ino: u64, flags: u32, writeback: bool) -> Result<(u64, u32), v1::FsError> {
    let node = self.get_inode(ino)?;
    if node.kind == v1::FsFileType::Symlink {
      return Err(fs_err(errno::ELOOP, "cannot open symlink directly"));
    }
    let adjusted = adjust_writeback_flags(flags, writeback);
    let host_flags = linux_open_flags_to_host(adjusted) | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let writable = (host_flags & libc::O_ACCMODE) != libc::O_RDONLY;

    let fd = if node.kind == v1::FsFileType::Directory {
      let dir_fd = self.ensure_dir_fd(ino)?;
      let dup = unsafe { libc::fcntl(dir_fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
      if dup < 0 {
        return Err(io_to_fs_err(io::Error::last_os_error()));
      }
      unsafe { OwnedFd::from_raw_fd(dup) }
    } else {
      let (parent, name) = node_loc(&node)?;
      let pfd = self.ensure_dir_fd(parent)?;
      let raw = unsafe { libc::openat(pfd.as_raw_fd(), name.as_ptr(), host_flags) };
      if raw < 0 {
        return Err(io_to_fs_err(io::Error::last_os_error()));
      }
      unsafe { OwnedFd::from_raw_fd(raw) }
    };

    let fh = self.insert_handle(ino, fd, writable && node.kind != v1::FsFileType::Directory);
    Ok((fh, adjusted))
  }

  pub(crate) fn read(&self, _ino: u64, fh: u64, offset: u64, size: usize) -> Result<(Vec<u8>, bool), v1::FsError> {
    let handle = self.get_handle(fh)?;
    if size == 0 {
      return Ok((Vec::new(), false));
    }
    let mut buf = vec![0u8; size];
    let n = unsafe { libc::pread(handle.fd.as_raw_fd(), buf.as_mut_ptr().cast(), size, offset as libc::off_t) };
    if n < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    let n = n as usize;
    buf.truncate(n);
    Ok((buf, n < size))
  }

  pub(crate) fn write(&self, _ino: u64, fh: u64, offset: u64, data: &[u8]) -> Result<u32, v1::FsError> {
    let handle = self.get_handle(fh)?;
    let written = pwrite_all(handle.fd.as_raw_fd(), data, offset)?;
    Ok(written as u32)
  }

  pub(crate) fn flush(&self, _ino: u64, fh: u64, _lock_owner: u64) -> Result<(), v1::FsError> {
    let handle = self.get_handle(fh)?;
    let dup_fd = unsafe { libc::dup(handle.fd.as_raw_fd()) };
    if dup_fd < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    let rc = unsafe { libc::close(dup_fd) };
    if rc < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    Ok(())
  }

  pub(crate) fn fsync(&self, _ino: u64, fh: u64, _datasync: bool) -> Result<(), v1::FsError> {
    let handle = self.get_handle(fh)?;
    let rc = unsafe { libc::fsync(handle.fd.as_raw_fd()) };
    if rc < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    Ok(())
  }

  pub(crate) fn release(&self, ino: u64, fh: u64, lock_owner: u64) -> Result<(), v1::FsError> {
    let removed = {
      let mut handles = self.handles.write().unwrap_or_else(|e| e.into_inner());
      handles.remove(&fh)
    };
    if lock_owner != 0 {
      let all_released = self.locks.release_owner(ino, lock_owner);
      if all_released && let Some(ref h) = removed {
        let fl = libc::flock {
          l_type: libc::F_UNLCK as libc::c_short,
          l_whence: libc::SEEK_SET as libc::c_short,
          l_start: 0,
          l_len: 0,
          l_pid: 0,
        };
        let _ = unsafe { libc::fcntl(h.fd.as_raw_fd(), libc::F_SETLK, &fl) };
      }
    }
    Ok(())
  }

  pub(crate) fn create(
    &self,
    parent: u64,
    name: FileName<'_>,
    mode: u32,
    flags: u32,
    writeback: bool,
  ) -> Result<(v1::FsAttr, u64, u32), v1::FsError> {
    let dir_fd = self.ensure_dir_fd(parent)?;
    let c_name = name_to_cstring(name)?;
    let adjusted = adjust_writeback_flags(flags, writeback);
    let host_flags = linux_open_flags_to_host(adjusted) | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let writable = (host_flags & libc::O_ACCMODE) != libc::O_RDONLY;
    let raw = unsafe { libc::openat(dir_fd.as_raw_fd(), c_name.as_ptr(), host_flags, (mode & 0o7777) as libc::c_uint) };
    if raw < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    let file_fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let st = fstat_fd(file_fd.as_raw_fd())?;

    let attr = self.register_inode(parent, c_name, &st)?;
    let fh = self.insert_handle(attr.ino, file_fd, writable);
    Ok((attr, fh, adjusted))
  }

  pub(crate) fn create_and_write(&self, parent: u64, name: FileName<'_>, mode: u32, data: &[u8]) -> Result<v1::FsAttr, v1::FsError> {
    let dir_fd = self.ensure_dir_fd(parent)?;
    let c_name = name_to_cstring(name)?;
    let raw = unsafe {
      libc::openat(
        dir_fd.as_raw_fd(),
        c_name.as_ptr(),
        libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        (mode & 0o7777) as libc::c_uint,
      )
    };
    if raw < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    let file_fd = unsafe { OwnedFd::from_raw_fd(raw) };
    pwrite_all(file_fd.as_raw_fd(), data, 0)?;
    let st = fstat_fd(file_fd.as_raw_fd())?;
    drop(file_fd);

    self.register_inode(parent, c_name, &st)
  }

  pub(crate) fn mkdir(&self, parent: u64, name: FileName<'_>, mode: u32) -> Result<v1::FsAttr, v1::FsError> {
    let dir_fd = self.ensure_dir_fd(parent)?;
    let c_name = name_to_cstring(name)?;
    let rc = unsafe { libc::mkdirat(dir_fd.as_raw_fd(), c_name.as_ptr(), (mode & 0o7777) as libc::mode_t) };
    if rc < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    self.lookup(parent, name)
  }

  pub(crate) fn unlink(&self, parent: u64, name: FileName<'_>) -> Result<(), v1::FsError> {
    let dir_fd = self.ensure_dir_fd(parent)?;
    let c_name = name_to_cstring(name)?;
    let rc = unsafe { libc::unlinkat(dir_fd.as_raw_fd(), c_name.as_ptr(), 0) };
    if rc < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    Ok(())
  }

  pub(crate) fn rmdir(&self, parent: u64, name: FileName<'_>) -> Result<(), v1::FsError> {
    let dir_fd = self.ensure_dir_fd(parent)?;
    let c_name = name_to_cstring(name)?;
    let target_key = fstatat_nofollow(dir_fd.as_raw_fd(), &c_name).ok().map(|st| stat_dev_ino(&st));
    let rc = unsafe { libc::unlinkat(dir_fd.as_raw_fd(), c_name.as_ptr(), libc::AT_REMOVEDIR) };
    if rc < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    if let Some(key) = target_key {
      let target_ino = {
        let store = self.inodes.read().unwrap_or_else(|e| e.into_inner());
        store.by_key.get(&key).copied()
      };
      if let Some(ino) = target_ino {
        self.evict_dir_fd(ino);
      }
    }
    Ok(())
  }

  pub(crate) fn rename(
    &self,
    parent: u64,
    name: FileName<'_>,
    new_parent: u64,
    new_name: FileName<'_>,
    flags: u32,
  ) -> Result<(), v1::FsError> {
    let old_fd = self.ensure_dir_fd(parent)?;
    let new_fd = self.ensure_dir_fd(new_parent)?;
    let c_old = name_to_cstring(name)?;
    let c_new = name_to_cstring(new_name)?;

    // Capture source `(dev, ino)` before rename so we can update its `loc`.
    let src_key = fstatat_nofollow(old_fd.as_raw_fd(), &c_old).ok().map(|st| stat_dev_ino(&st));

    if flags == 0 {
      let rc = unsafe { libc::renameat(old_fd.as_raw_fd(), c_old.as_ptr(), new_fd.as_raw_fd(), c_new.as_ptr()) };
      if rc < 0 {
        return Err(io_to_fs_err(io::Error::last_os_error()));
      }
    } else {
      #[cfg(target_os = "macos")]
      {
        let mut mac_flags: libc::c_uint = 0;
        if (flags & linux_flags::RENAME_NOREPLACE) != 0 {
          mac_flags |= libc::RENAME_EXCL;
        }
        if (flags & linux_flags::RENAME_EXCHANGE) != 0 {
          mac_flags |= libc::RENAME_SWAP;
        }
        let known = linux_flags::RENAME_NOREPLACE | linux_flags::RENAME_EXCHANGE;
        if (flags & !known) != 0 {
          return Err(fs_err(errno::EINVAL, "unsupported rename flags"));
        }
        let rc = unsafe { libc::renameatx_np(old_fd.as_raw_fd(), c_old.as_ptr(), new_fd.as_raw_fd(), c_new.as_ptr(), mac_flags) };
        if rc < 0 {
          return Err(io_to_fs_err(io::Error::last_os_error()));
        }
      }
      #[cfg(not(target_os = "macos"))]
      {
        return Err(fs_err(errno::EOPNOTSUPP, "rename flags not supported"));
      }
    }

    if let Some(key) = src_key {
      let store = self.inodes.read().unwrap_or_else(|e| e.into_inner());
      if let Some(&ino) = store.by_key.get(&key)
        && let Some(node) = store.by_ino.get(&ino)
      {
        *node.loc.lock().unwrap_or_else(|e| e.into_inner()) = Some((new_parent, c_new));
      }
    }
    Ok(())
  }

  pub(crate) fn symlink(&self, parent: u64, name: FileName<'_>, link_target: &str) -> Result<v1::FsAttr, v1::FsError> {
    let dir_fd = self.ensure_dir_fd(parent)?;
    let c_name = name_to_cstring(name)?;
    let c_target = CString::new(link_target).map_err(|_| fs_err(errno::EINVAL, "invalid symlink target"))?;
    let rc = unsafe { libc::symlinkat(c_target.as_ptr(), dir_fd.as_raw_fd(), c_name.as_ptr()) };
    if rc < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    self.lookup(parent, name)
  }

  pub(crate) fn readlink(&self, ino: u64) -> Result<String, v1::FsError> {
    let node = self.get_inode(ino)?;
    let (parent, name) = node_loc(&node)?;
    let pfd = self.ensure_dir_fd(parent)?;
    let mut buf = vec![0u8; 4096];
    let n = unsafe { libc::readlinkat(pfd.as_raw_fd(), name.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    buf.truncate(n as usize);
    String::from_utf8(buf).map_err(|_| fs_err(errno::EIO, "non-utf8 symlink target"))
  }

  pub(crate) fn link(&self, ino: u64, new_parent: u64, new_name: FileName<'_>) -> Result<v1::FsAttr, v1::FsError> {
    let node = self.get_inode(ino)?;
    let (old_parent, old_name) = node_loc(&node)?;
    let old_fd = self.ensure_dir_fd(old_parent)?;
    let new_fd = self.ensure_dir_fd(new_parent)?;
    let c_new = name_to_cstring(new_name)?;
    let rc = unsafe { libc::linkat(old_fd.as_raw_fd(), old_name.as_ptr(), new_fd.as_raw_fd(), c_new.as_ptr(), 0) };
    if rc < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    self.lookup(new_parent, new_name)
  }

  pub(crate) fn statfs(&self, ino: u64) -> Result<v1::FsStatFsReply, v1::FsError> {
    let node = self.get_inode(ino)?;
    let fd = if node.kind == v1::FsFileType::Directory {
      self.ensure_dir_fd(ino)?
    } else {
      Arc::clone(&self.root_fd)
    };
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::fstatvfs(fd.as_raw_fd(), &mut st) };
    if rc < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    Ok(v1::FsStatFsReply {
      blocks: st.f_blocks as u64,
      bfree: st.f_bfree as u64,
      bavail: st.f_bavail as u64,
      files: st.f_files as u64,
      ffree: st.f_ffree as u64,
      bsize: st.f_bsize as u32,
      namelen: st.f_namemax as u32,
      frsize: st.f_frsize as u32,
    })
  }

  pub(crate) fn forget(&self, items: &[v1::FsForgetItem]) {
    let mut evicted_dirs = Vec::new();
    {
      let mut store = self.inodes.write().unwrap_or_else(|e| e.into_inner());
      for item in items {
        if item.ino == ROOT_INO || item.nlookup == 0 {
          continue;
        }
        let evict = if let Some(entry) = store.by_ino.get(&item.ino) {
          entry.dec_ref(item.nlookup) <= item.nlookup
        } else {
          false
        };
        if evict && let Some(removed) = store.by_ino.remove(&item.ino) {
          if store.by_key.get(&removed.dev_ino) == Some(&item.ino) {
            store.by_key.remove(&removed.dev_ino);
          }
          if removed.kind == v1::FsFileType::Directory {
            evicted_dirs.push(item.ino);
          }
        }
      }
    }
    if !evicted_dirs.is_empty() {
      let mut cache = self.dir_fds.lock().unwrap_or_else(|e| e.into_inner());
      for ino in evicted_dirs {
        cache.remove(ino);
      }
    }
  }

  pub(crate) fn getlock(&self, ino: u64, fh: u64, lock_owner: u64, range: v1::FsLockRange) -> Result<v1::FsLockRange, v1::FsError> {
    let handle = self.get_handle(fh)?;
    let effective_owner = if lock_owner != 0 { lock_owner } else { fh };

    // Check in-process multi-handle conflicts first (POSIX `fcntl` does not
    // conflict within the same process).
    let local = self.locks.get_lock(ino, effective_owner, range)?;
    if local.kind != v1::FsLockType::Unlock as i32 {
      return Ok(local);
    }

    // Also query the OS kernel for locks held by external host processes.
    let mut fl = range_to_flock(&range)?;
    let rc = unsafe { libc::fcntl(handle.fd.as_raw_fd(), libc::F_GETLK, &mut fl) };
    if rc < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    Ok(flock_to_range(&fl, &range))
  }

  pub(crate) fn setlock(&self, ino: u64, fh: u64, lock_owner: u64, range: v1::FsLockRange, sleep: bool) -> Result<(), v1::FsError> {
    let handle = self.get_handle(fh)?;
    let effective_owner = if lock_owner != 0 { lock_owner } else { fh };
    let fl = range_to_flock(&range)?;

    self.locks.set_lock(ino, effective_owner, range)?;

    // Mirror to OS `fcntl` for visibility to external host processes.
    let cmd = if sleep { libc::F_SETLKW } else { libc::F_SETLK };
    let rc = unsafe { libc::fcntl(handle.fd.as_raw_fd(), cmd, &fl) };
    if rc < 0 {
      let err = io::Error::last_os_error();
      // EBADF happens if the handle was opened read-only for a write lock;
      // roll back the local entry and surface the OS error.
      let _ = self.locks.set_lock(
        ino,
        effective_owner,
        v1::FsLockRange {
          kind: v1::FsLockType::Unlock as i32,
          ..range
        },
      );
      return Err(io_to_fs_err(err));
    }
    Ok(())
  }
}

fn node_loc(node: &InodeData) -> Result<(u64, CString), v1::FsError> {
  node.loc.lock().unwrap_or_else(|e| e.into_inner()).clone().ok_or_else(|| fs_err(errno::ENOENT, "inode has no parent path"))
}

fn is_safe_inode(mode: libc::mode_t) -> bool {
  matches!(mode & libc::S_IFMT, libc::S_IFREG | libc::S_IFDIR | libc::S_IFLNK)
}

fn mode_to_file_type(mode: libc::mode_t) -> v1::FsFileType {
  match mode & libc::S_IFMT {
    libc::S_IFREG => v1::FsFileType::Regular,
    libc::S_IFDIR => v1::FsFileType::Directory,
    libc::S_IFLNK => v1::FsFileType::Symlink,
    _ => v1::FsFileType::Unspecified,
  }
}

#[allow(clippy::unnecessary_cast)]
fn stat_dev_ino(st: &libc::stat) -> DevIno {
  (st.st_dev as u64, st.st_ino as u64)
}

#[allow(clippy::unnecessary_cast)]
fn stat_to_attr(ino: u64, st: &libc::stat) -> v1::FsAttr {
  v1::FsAttr {
    ino,
    size: st.st_size.max(0) as u64,
    blocks: st.st_blocks.max(0) as u64,
    atime_sec: st.st_atime as i64,
    atime_nsec: st.st_atime_nsec.max(0) as u32,
    mtime_sec: st.st_mtime as i64,
    mtime_nsec: st.st_mtime_nsec.max(0) as u32,
    ctime_sec: st.st_ctime as i64,
    ctime_nsec: st.st_ctime_nsec.max(0) as u32,
    kind: mode_to_file_type(st.st_mode) as i32,
    perm: (st.st_mode & 0o7777) as u32,
    nlink: st.st_nlink as u32,
    uid: st.st_uid,
    gid: st.st_gid,
    blksize: st.st_blksize.max(0) as u32,
  }
}

fn fstat_fd(fd: RawFd) -> Result<libc::stat, v1::FsError> {
  let mut st: libc::stat = unsafe { std::mem::zeroed() };
  let rc = unsafe { libc::fstat(fd, &mut st) };
  if rc < 0 {
    return Err(io_to_fs_err(io::Error::last_os_error()));
  }
  Ok(st)
}

fn fstatat_nofollow(dirfd: RawFd, name: &CStr) -> Result<libc::stat, v1::FsError> {
  let mut st: libc::stat = unsafe { std::mem::zeroed() };
  let rc = unsafe { libc::fstatat(dirfd, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) };
  if rc < 0 {
    return Err(io_to_fs_err(io::Error::last_os_error()));
  }
  Ok(st)
}

fn detect_case_insensitive(dir_fd: RawFd) -> bool {
  #[cfg(target_os = "macos")]
  {
    let val = unsafe { libc::fpathconf(dir_fd, libc::_PC_CASE_SENSITIVE) };
    val == 0
  }
  #[cfg(not(target_os = "macos"))]
  {
    let _ = dir_fd;
    false
  }
}

/// Translate Linux wire `open(2)` flags to host macOS/BSD `libc::O_*` flags.
fn linux_open_flags_to_host(flags: u32) -> libc::c_int {
  let mut out = match flags & linux_flags::O_ACCMODE {
    linux_flags::O_WRONLY => libc::O_WRONLY,
    linux_flags::O_RDWR => libc::O_RDWR,
    _ => libc::O_RDONLY,
  };
  if (flags & linux_flags::O_CREAT) != 0 {
    out |= libc::O_CREAT;
  }
  if (flags & linux_flags::O_EXCL) != 0 {
    out |= libc::O_EXCL;
  }
  if (flags & linux_flags::O_TRUNC) != 0 {
    out |= libc::O_TRUNC;
  }
  if (flags & linux_flags::O_APPEND) != 0 {
    out |= libc::O_APPEND;
  }
  if (flags & linux_flags::O_NONBLOCK) != 0 {
    out |= libc::O_NONBLOCK;
  }
  if (flags & linux_flags::O_SYNC) != 0 {
    out |= libc::O_SYNC;
  }
  out
}

fn pwrite_all(fd: RawFd, mut data: &[u8], mut offset: u64) -> Result<usize, v1::FsError> {
  let mut total = 0usize;
  while !data.is_empty() {
    let n = unsafe { libc::pwrite(fd, data.as_ptr().cast(), data.len(), offset as libc::off_t) };
    if n < 0 {
      let err = io::Error::last_os_error();
      if err.raw_os_error() == Some(libc::EINTR) {
        continue;
      }
      return Err(io_to_fs_err(err));
    }
    let n = n as usize;
    total += n;
    offset += n as u64;
    data = &data[n..];
  }
  Ok(total)
}

fn range_to_flock(range: &v1::FsLockRange) -> Result<libc::flock, v1::FsError> {
  let kind = v1::FsLockType::try_from(range.kind).unwrap_or(v1::FsLockType::Unspecified);
  let l_type = match kind {
    v1::FsLockType::Read => libc::F_RDLCK,
    v1::FsLockType::Write => libc::F_WRLCK,
    v1::FsLockType::Unlock => libc::F_UNLCK,
    v1::FsLockType::Unspecified => return Err(fs_err(errno::EINVAL, "unspecified lock type")),
  };
  if range.end < range.start {
    return Err(fs_err(errno::EINVAL, "lock end before start"));
  }
  let l_len = if range.end == u64::MAX {
    0
  } else {
    range.end.saturating_sub(range.start).saturating_add(1).min(i64::MAX as u64) as libc::off_t
  };
  Ok(libc::flock {
    l_start: range.start.min(i64::MAX as u64) as libc::off_t,
    l_len,
    l_pid: 0,
    l_type: l_type as libc::c_short,
    l_whence: libc::SEEK_SET as libc::c_short,
  })
}

fn flock_to_range(fl: &libc::flock, requested: &v1::FsLockRange) -> v1::FsLockRange {
  if fl.l_type == libc::F_UNLCK as libc::c_short {
    return v1::FsLockRange {
      start: requested.start,
      end: requested.end,
      kind: v1::FsLockType::Unlock as i32,
      pid: 0,
    };
  }
  let start = fl.l_start.max(0) as u64;
  let end = if fl.l_len == 0 {
    u64::MAX
  } else {
    start.saturating_add(fl.l_len.max(1) as u64 - 1)
  };
  let kind = if fl.l_type == libc::F_WRLCK as libc::c_short {
    v1::FsLockType::Write as i32
  } else {
    v1::FsLockType::Read as i32
  };
  v1::FsLockRange {
    start,
    end,
    kind,
    pid: fl.l_pid.max(0) as u32,
  }
}

fn name_to_cstring(name: FileName<'_>) -> Result<CString, v1::FsError> {
  CString::new(name.as_str()).map_err(|_| fs_err(errno::EINVAL, "filename contains NUL"))
}

fn path_to_cstring(path: &Path) -> Result<CString, v1::FsError> {
  CString::new(OsStr::as_bytes(path.as_os_str())).map_err(|_| fs_err(errno::EINVAL, "path contains NUL"))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn regular_file_walk_pins_zero_file_fds_and_bounds_dir_fds() {
    let tmp = tempfile::tempdir().unwrap();
    let num_dirs = 30usize;
    let total_files = 500usize;
    for d in 0..num_dirs {
      std::fs::create_dir(tmp.path().join(format!("dir_{d:02}"))).unwrap();
    }
    for i in 0..total_files {
      let d = i % num_dirs;
      let path = tmp.path().join(format!("dir_{d:02}")).join(format!("file_{i:04}.txt"));
      std::fs::write(&path, format!("payload-{i}")).unwrap();
    }

    let fs = UnixFs::open(tmp.path()).unwrap();
    assert_eq!(fs.pinned_fd_count(), 1); // root only

    let mut seen_files = 0usize;
    for d in 0..num_dirs {
      let dname = format!("dir_{d:02}");
      let dir_attr = fs.lookup(ROOT_INO, FileName::parse(&dname).unwrap()).unwrap();
      let (entries, eof) = fs.readdirplus(dir_attr.ino, 0, 256, 1).unwrap();
      assert!(eof);
      for ent in entries {
        let attr = fs.getattr(ent.ino, None).unwrap();
        assert_eq!(attr.kind, v1::FsFileType::Regular as i32);
        seen_files += 1;
      }
    }
    assert_eq!(seen_files, total_files);

    // Also exercise `create_and_write` and `create` + `release` on regular files.
    let dir0 = fs.lookup(ROOT_INO, FileName::parse("dir_00").unwrap()).unwrap();
    let cw = fs.create_and_write(dir0.ino, FileName::parse("extra_cw.txt").unwrap(), 0o644, b"extra").unwrap();
    assert_eq!(fs.getattr(cw.ino, None).unwrap().size, 5);
    let (cr_attr, fh, _) = fs.create(dir0.ino, FileName::parse("extra_cr.txt").unwrap(), 0o644, linux_flags::O_RDWR, false).unwrap();
    fs.write(cr_attr.ino, fh, 0, b"created").unwrap();
    fs.release(cr_attr.ino, fh, 0).unwrap();

    // 500+ regular files walked and created -> 0 regular-file FDs pinned, only
    // root + 30 subdirectories (31 <= 32).
    assert_eq!(fs.handles.read().unwrap().len(), 0);
    assert_eq!(fs.pinned_fd_count(), 1 + num_dirs);
    assert!(fs.pinned_fd_count() <= 32);
  }

  #[test]
  fn dir_fd_lru_cache_bounds_fds_and_reopens_transparently() {
    let tmp = tempfile::tempdir().unwrap();

    // Build 10 nested directories (`nest_0/.../nest_9`) + 10 sibling directories (`sib_0`..`sib_9`).
    let mut nested_path = tmp.path().to_path_buf();
    for i in 0..10 {
      nested_path = nested_path.join(format!("nest_{i}"));
      std::fs::create_dir(&nested_path).unwrap();
      std::fs::write(nested_path.join("data.txt"), format!("nested-{i}")).unwrap();
    }
    for i in 0..10 {
      let sib = tmp.path().join(format!("sib_{i}"));
      std::fs::create_dir(&sib).unwrap();
      std::fs::write(sib.join("data.txt"), format!("sibling-{i}")).unwrap();
    }

    let cap = 4usize;
    let fs = UnixFs::open_with_dir_cache_cap(tmp.path(), cap).unwrap();
    assert_eq!(fs.pinned_fd_count(), 1);

    let mut nested_inos = Vec::with_capacity(10);
    let mut parent = ROOT_INO;
    for i in 0..10 {
      let name = format!("nest_{i}");
      let attr = fs.lookup(parent, FileName::parse(&name).unwrap()).unwrap();
      assert!(fs.pinned_fd_count() <= 1 + cap, "pinned_fd_count={} exceeded {}", fs.pinned_fd_count(), 1 + cap);
      nested_inos.push(attr.ino);
      parent = attr.ino;
    }

    let mut sib_inos = Vec::with_capacity(10);
    for i in 0..10 {
      let name = format!("sib_{i}");
      let attr = fs.lookup(ROOT_INO, FileName::parse(&name).unwrap()).unwrap();
      assert!(fs.pinned_fd_count() <= 1 + cap, "pinned_fd_count={} exceeded {}", fs.pinned_fd_count(), 1 + cap);
      sib_inos.push(attr.ino);
    }

    // Re-access earliest evicted nested & sibling directories (lookup, read, write).
    for (idx, &dir_ino) in nested_inos.iter().enumerate() {
      let f_attr = fs.lookup(dir_ino, FileName::parse("data.txt").unwrap()).unwrap();
      let (fh, _) = fs.open_file(f_attr.ino, linux_flags::O_RDONLY, false).unwrap();
      let (bytes, _) = fs.read(f_attr.ino, fh, 0, 64).unwrap();
      fs.release(f_attr.ino, fh, 0).unwrap();
      assert_eq!(bytes, format!("nested-{idx}").as_bytes());
      assert!(fs.pinned_fd_count() <= 1 + cap);
    }
    for (idx, &dir_ino) in sib_inos.iter().enumerate() {
      let f_attr = fs.lookup(dir_ino, FileName::parse("data.txt").unwrap()).unwrap();
      let (fh, _) = fs.open_file(f_attr.ino, linux_flags::O_RDONLY, false).unwrap();
      let (bytes, _) = fs.read(f_attr.ino, fh, 0, 64).unwrap();
      fs.release(f_attr.ino, fh, 0).unwrap();
      assert_eq!(bytes, format!("sibling-{idx}").as_bytes());
      assert!(fs.pinned_fd_count() <= 1 + cap);
    }

    // Write into an evicted deep nested directory (`nest_9` was evicted while reading `sib_*`).
    let deep_dir = nested_inos[9];
    let created = fs.create_and_write(deep_dir, FileName::parse("after_evict.txt").unwrap(), 0o644, b"ok").unwrap();
    assert_eq!(fs.getattr(created.ino, None).unwrap().size, 2);
    assert!(fs.pinned_fd_count() <= 1 + cap);

    // `rmdir` evicts the directory FD from the cache.
    let ephemeral = fs.mkdir(ROOT_INO, FileName::parse("ephemeral_dir").unwrap(), 0o755).unwrap();
    let before_rmdir = fs.pinned_fd_count();
    fs.rmdir(ROOT_INO, FileName::parse("ephemeral_dir").unwrap()).unwrap();
    assert_eq!(fs.pinned_fd_count(), before_rmdir - 1);
    let _ = ephemeral;

    // `forget` evicts the directory FD from the cache when refcount reaches 0.
    let to_forget = fs.mkdir(ROOT_INO, FileName::parse("forget_dir").unwrap(), 0o755).unwrap();
    let before_forget = fs.pinned_fd_count();
    fs.forget(&[v1::FsForgetItem {
      ino: to_forget.ino,
      nlookup: 1,
    }]);
    assert_eq!(fs.pinned_fd_count(), before_forget - 1);
  }

  #[test]
  fn open_unlinked_file_getattr_and_setattr_work_without_fh() {
    let tmp = tempfile::tempdir().unwrap();
    let fs = UnixFs::open(tmp.path()).unwrap();

    let (attr, fh, _) = fs.create(ROOT_INO, FileName::parse("unlinked.txt").unwrap(), 0o644, linux_flags::O_RDWR, false).unwrap();
    fs.write(attr.ino, fh, 0, b"hello world").unwrap();
    fs.unlink(ROOT_INO, FileName::parse("unlinked.txt").unwrap()).unwrap();

    // `getattr(ino, None)` falls back to the open handle for `ino`.
    let got = fs.getattr(attr.ino, None).unwrap();
    assert_eq!(got.size, 11);

    // `setattr(ino, fh: None, ...)` also succeeds via the open handle.
    let updated = fs
      .setattr(&v1::FsSetAttr {
        ino: attr.ino,
        fh: None,
        mode: Some(0o600),
        size: Some(5),
        atime_sec: Some(1_700_000_000),
        atime_nsec: Some(0),
        mtime_sec: Some(1_700_000_123),
        mtime_nsec: Some(0),
      })
      .unwrap();
    assert_eq!(updated.size, 5);
    assert_eq!(updated.perm & 0o777, 0o600);
    assert_eq!(updated.mtime_sec, 1_700_000_123);

    let (data, _) = fs.read(attr.ino, fh, 0, 16).unwrap();
    assert_eq!(data, b"hello");

    fs.release(attr.ino, fh, 0).unwrap();

    // Once closed and unlinked, `getattr(ino, None)` returns ENOENT.
    let err = fs.getattr(attr.ino, None).unwrap_err();
    assert_eq!(err.errno, errno::ENOENT);
  }
}
