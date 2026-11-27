//! Linux executor filesystem server (`PassthroughFs`), adapted from
//! `virtiofsd`:
//! - `openat2(RESOLVE_IN_ROOT | RESOLVE_NO_MAGICLINKS)` with `O_NOFOLLOW |
//!   O_CLOEXEC` (falling back to `openat` on `ENOSYS`).
//! - `O_PATH` fd pinning in a `(dev, ino)` store with non-zero CAS refcounting.
//! - `is_safe_inode` gate allowing only regular files, directories and symlinks.
//! - Reopen via `/proc/self/fd/{fd}` with writeback flag promotion.
//! - `dup()` + `close()` on `Flush` to surface deferred write errors.
//! - Per-handle `F_OFD_GETLK` / `F_OFD_SETLK` / `F_OFD_SETLKW` (`l_pid = 0`).

use std::collections::HashMap;
use std::ffi::{CStr, CString, OsStr};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use needy_api_proto::v1;

use super::{FileName, ROOT_INO, adjust_writeback_flags, errno, fs_err, io_to_fs_err, linux_flags};

// Source: https://github.com/torvalds/linux/blob/v6.12/include/uapi/linux/openat2.h#L18-L45
// Reference: Linux `openat2(2)`
#[repr(C)]
struct OpenHow {
  flags: u64,
  mode: u64,
  resolve: u64,
}

const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
const RESOLVE_IN_ROOT: u64 = 0x10;

// Source: https://github.com/torvalds/linux/blob/v6.12/include/uapi/asm-generic/fcntl.h#L168-L176
// Reference: Linux `fcntl(2)` Open File Description locks
const F_OFD_GETLK: libc::c_int = 36;
const F_OFD_SETLK: libc::c_int = 37;
const F_OFD_SETLKW: libc::c_int = 38;

type DevIno = (u64, u64);

struct InodeData {
  dev_ino: DevIno,
  fd: OwnedFd,
  kind: v1::FsFileType,
  refcount: AtomicU64,
}

impl InodeData {
  /// Non-zero CAS increment: forbids `0 -> 1` resurrection of an evicted inode.
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

struct HandleData {
  fd: OwnedFd,
}

pub(crate) struct PassthroughFs {
  root_attr: v1::FsAttr,
  inodes: RwLock<InodeStore>,
  handles: RwLock<HashMap<u64, Arc<HandleData>>>,
  next_ino: AtomicU64,
  next_fh: AtomicU64,
}

impl PassthroughFs {
  pub(crate) fn open(root: &Path) -> Result<Self, v1::FsError> {
    let c_root = path_to_cstring(root)?;
    let raw = unsafe { libc::open(c_root.as_ptr(), libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC) };
    if raw < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let st = fstat_fd(fd.as_raw_fd())?;
    if !is_safe_inode(st.st_mode) || (st.st_mode & libc::S_IFMT) != libc::S_IFDIR {
      return Err(fs_err(errno::ENOTDIR, "workspace root is not a directory"));
    }

    let dev_ino = stat_dev_ino(&st);
    let root_attr = stat_to_attr(ROOT_INO, &st);
    let root_data = Arc::new(InodeData {
      dev_ino,
      fd,
      kind: v1::FsFileType::Directory,
      refcount: AtomicU64::new(1),
    });

    let mut by_ino = HashMap::new();
    let mut by_key = HashMap::new();
    by_ino.insert(ROOT_INO, root_data);
    by_key.insert(dev_ino, ROOT_INO);

    Ok(Self {
      root_attr,
      inodes: RwLock::new(InodeStore { by_ino, by_key }),
      handles: RwLock::new(HashMap::new()),
      next_ino: AtomicU64::new(ROOT_INO + 1),
      next_fh: AtomicU64::new(1),
    })
  }

  pub(crate) fn root_attr(&self) -> v1::FsAttr {
    self.root_attr
  }

  pub(crate) fn case_insensitive(&self) -> bool {
    false
  }

  fn get_inode(&self, ino: u64) -> Result<Arc<InodeData>, v1::FsError> {
    let store = self.inodes.read().unwrap_or_else(|e| e.into_inner());
    store.by_ino.get(&ino).cloned().ok_or_else(|| fs_err(errno::ENOENT, format!("unknown inode {ino}")))
  }

  fn get_handle(&self, fh: u64) -> Result<Arc<HandleData>, v1::FsError> {
    let handles = self.handles.read().unwrap_or_else(|e| e.into_inner());
    handles.get(&fh).cloned().ok_or_else(|| fs_err(errno::EBADF, format!("unknown file handle {fh}")))
  }

  fn insert_handle(&self, fd: OwnedFd) -> u64 {
    let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
    let mut handles = self.handles.write().unwrap_or_else(|e| e.into_inner());
    handles.insert(fh, Arc::new(HandleData { fd }));
    fh
  }

  fn pin_child_fd(&self, fd: OwnedFd, st: &libc::stat) -> Result<v1::FsAttr, v1::FsError> {
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
        return Ok(stat_to_attr(ino, st));
      }
    }

    let mut store = self.inodes.write().unwrap_or_else(|e| e.into_inner());
    if let Some(&ino) = store.by_key.get(&dev_ino)
      && let Some(existing) = store.by_ino.get(&ino)
      && existing.try_inc_ref()
    {
      return Ok(stat_to_attr(ino, st));
    }

    let ino = self.next_ino.fetch_add(1, Ordering::Relaxed);
    let data = Arc::new(InodeData {
      dev_ino,
      fd,
      kind,
      refcount: AtomicU64::new(1),
    });
    store.by_ino.insert(ino, data);
    store.by_key.insert(dev_ino, ino);
    Ok(stat_to_attr(ino, st))
  }

  pub(crate) fn lookup(&self, parent: u64, name: FileName<'_>) -> Result<v1::FsAttr, v1::FsError> {
    let dir = self.get_inode(parent)?;
    if dir.kind != v1::FsFileType::Directory {
      return Err(fs_err(errno::ENOTDIR, "parent is not a directory"));
    }
    let c_name = name_to_cstring(name)?;
    let child_fd = openat2_in_root(dir.fd.as_raw_fd(), &c_name, (libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC) as u64, 0)?;
    let st = fstat_fd(child_fd.as_raw_fd())?;
    self.pin_child_fd(child_fd, &st)
  }

  pub(crate) fn getattr(&self, ino: u64, fh: Option<u64>) -> Result<v1::FsAttr, v1::FsError> {
    if let Some(fh) = fh {
      let handle = self.get_handle(fh)?;
      let st = fstat_fd(handle.fd.as_raw_fd())?;
      return Ok(stat_to_attr(ino, &st));
    }
    let node = self.get_inode(ino)?;
    let st = fstat_fd(node.fd.as_raw_fd())?;
    Ok(stat_to_attr(ino, &st))
  }

  pub(crate) fn setattr(&self, req: &v1::FsSetAttr) -> Result<v1::FsAttr, v1::FsError> {
    let handle = match req.fh {
      Some(fh) => Some(self.get_handle(fh)?),
      None => None,
    };
    let node = self.get_inode(req.ino)?;
    let proc_path = proc_fd_path(node.fd.as_raw_fd());

    if let Some(mode) = req.mode {
      let rc = if let Some(ref h) = handle {
        unsafe { libc::fchmod(h.fd.as_raw_fd(), (mode & 0o7777) as libc::mode_t) }
      } else {
        unsafe { libc::chmod(proc_path.as_ptr(), (mode & 0o7777) as libc::mode_t) }
      };
      if rc < 0 {
        return Err(io_to_fs_err(io::Error::last_os_error()));
      }
    }

    if let Some(size) = req.size {
      let rc = if let Some(ref h) = handle {
        unsafe { libc::ftruncate(h.fd.as_raw_fd(), size as libc::off_t) }
      } else {
        unsafe { libc::truncate(proc_path.as_ptr(), size as libc::off_t) }
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
      } else {
        unsafe { libc::utimensat(libc::AT_FDCWD, proc_path.as_ptr(), times.as_ptr(), 0) }
      };
      if rc < 0 {
        return Err(io_to_fs_err(io::Error::last_os_error()));
      }
    }

    let st = if let Some(ref h) = handle {
      fstat_fd(h.fd.as_raw_fd())?
    } else {
      fstat_fd(node.fd.as_raw_fd())?
    };
    Ok(stat_to_attr(req.ino, &st))
  }

  pub(crate) fn readdirplus(
    &self,
    ino: u64,
    offset: u64,
    max_entries: usize,
    generation: u64,
  ) -> Result<(Vec<v1::FsDirEntry>, bool), v1::FsError> {
    let dir = self.get_inode(ino)?;
    if dir.kind != v1::FsFileType::Directory {
      return Err(fs_err(errno::ENOTDIR, "not a directory"));
    }

    let reopened = reopen_opath(dir.fd.as_raw_fd(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC)?;
    let dir_ptr = unsafe { libc::fdopendir(reopened.as_raw_fd()) };
    if dir_ptr.is_null() {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    // `fdopendir` takes ownership of the fd.
    std::mem::forget(reopened);

    struct DirGuard(*mut libc::DIR);
    impl Drop for DirGuard {
      fn drop(&mut self) {
        unsafe { libc::closedir(self.0) };
      }
    }
    let guard = DirGuard(dir_ptr);

    let mut names: Vec<String> = Vec::new();
    loop {
      unsafe { *libc::__errno_location() = 0 };
      let ent = unsafe { libc::readdir(guard.0) };
      if ent.is_null() {
        let err = io::Error::last_os_error();
        if err.raw_os_error().unwrap_or(0) != 0 {
          return Err(io_to_fs_err(err));
        }
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
    let open_flags = (adjusted as libc::c_int) | libc::O_CLOEXEC;
    let fd = reopen_opath(node.fd.as_raw_fd(), open_flags)?;
    let fh = self.insert_handle(fd);
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
    // `dup()` + `close()` surfaces deferred host write errors without closing
    // the open file handle or releasing its `F_OFD_*` locks.
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

  pub(crate) fn fsync(&self, _ino: u64, fh: u64, datasync: bool) -> Result<(), v1::FsError> {
    let handle = self.get_handle(fh)?;
    let rc = unsafe {
      if datasync {
        libc::fdatasync(handle.fd.as_raw_fd())
      } else {
        libc::fsync(handle.fd.as_raw_fd())
      }
    };
    if rc < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    Ok(())
  }

  pub(crate) fn release(&self, _ino: u64, fh: u64, _lock_owner: u64) -> Result<(), v1::FsError> {
    let mut handles = self.handles.write().unwrap_or_else(|e| e.into_inner());
    handles.remove(&fh);
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
    let dir = self.get_inode(parent)?;
    if dir.kind != v1::FsFileType::Directory {
      return Err(fs_err(errno::ENOTDIR, "parent is not a directory"));
    }
    let c_name = name_to_cstring(name)?;
    let adjusted = adjust_writeback_flags(flags, writeback);
    let open_flags = ((adjusted as libc::c_int) | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC) as u64;
    let file_fd = openat2_in_root(dir.fd.as_raw_fd(), &c_name, open_flags, (mode & 0o7777) as u64)?;

    let opath_fd = openat2_in_root(dir.fd.as_raw_fd(), &c_name, (libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC) as u64, 0)?;
    let st = fstat_fd(file_fd.as_raw_fd())?;
    let attr = self.pin_child_fd(opath_fd, &st)?;
    let fh = self.insert_handle(file_fd);
    Ok((attr, fh, adjusted))
  }

  pub(crate) fn create_and_write(&self, parent: u64, name: FileName<'_>, mode: u32, data: &[u8]) -> Result<v1::FsAttr, v1::FsError> {
    let dir = self.get_inode(parent)?;
    if dir.kind != v1::FsFileType::Directory {
      return Err(fs_err(errno::ENOTDIR, "parent is not a directory"));
    }
    let c_name = name_to_cstring(name)?;
    let file_fd = openat2_in_root(
      dir.fd.as_raw_fd(),
      &c_name,
      (libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_NOFOLLOW | libc::O_CLOEXEC) as u64,
      (mode & 0o7777) as u64,
    )?;
    pwrite_all(file_fd.as_raw_fd(), data, 0)?;

    let opath_fd = openat2_in_root(dir.fd.as_raw_fd(), &c_name, (libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC) as u64, 0)?;
    let st = fstat_fd(file_fd.as_raw_fd())?;
    drop(file_fd);
    self.pin_child_fd(opath_fd, &st)
  }

  pub(crate) fn mkdir(&self, parent: u64, name: FileName<'_>, mode: u32) -> Result<v1::FsAttr, v1::FsError> {
    let dir = self.get_inode(parent)?;
    let c_name = name_to_cstring(name)?;
    let rc = unsafe { libc::mkdirat(dir.fd.as_raw_fd(), c_name.as_ptr(), (mode & 0o7777) as libc::mode_t) };
    if rc < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    self.lookup(parent, name)
  }

  pub(crate) fn unlink(&self, parent: u64, name: FileName<'_>) -> Result<(), v1::FsError> {
    let dir = self.get_inode(parent)?;
    let c_name = name_to_cstring(name)?;
    let rc = unsafe { libc::unlinkat(dir.fd.as_raw_fd(), c_name.as_ptr(), 0) };
    if rc < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    Ok(())
  }

  pub(crate) fn rmdir(&self, parent: u64, name: FileName<'_>) -> Result<(), v1::FsError> {
    let dir = self.get_inode(parent)?;
    let c_name = name_to_cstring(name)?;
    let rc = unsafe { libc::unlinkat(dir.fd.as_raw_fd(), c_name.as_ptr(), libc::AT_REMOVEDIR) };
    if rc < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
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
    let old_dir = self.get_inode(parent)?;
    let new_dir = self.get_inode(new_parent)?;
    let c_old = name_to_cstring(name)?;
    let c_new = name_to_cstring(new_name)?;
    let rc = if flags == 0 {
      unsafe { libc::renameat(old_dir.fd.as_raw_fd(), c_old.as_ptr(), new_dir.fd.as_raw_fd(), c_new.as_ptr()) }
    } else {
      unsafe {
        libc::syscall(
          libc::SYS_renameat2,
          old_dir.fd.as_raw_fd(),
          c_old.as_ptr(),
          new_dir.fd.as_raw_fd(),
          c_new.as_ptr(),
          flags as libc::c_uint,
        ) as libc::c_int
      }
    };
    if rc < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    Ok(())
  }

  pub(crate) fn symlink(&self, parent: u64, name: FileName<'_>, link_target: &str) -> Result<v1::FsAttr, v1::FsError> {
    let dir = self.get_inode(parent)?;
    let c_name = name_to_cstring(name)?;
    let c_target = CString::new(link_target).map_err(|_| fs_err(errno::EINVAL, "invalid symlink target"))?;
    let rc = unsafe { libc::symlinkat(c_target.as_ptr(), dir.fd.as_raw_fd(), c_name.as_ptr()) };
    if rc < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    self.lookup(parent, name)
  }

  pub(crate) fn readlink(&self, ino: u64) -> Result<String, v1::FsError> {
    let node = self.get_inode(ino)?;
    let empty = CString::new("").unwrap();
    let mut buf = vec![0u8; 4096];
    let n = unsafe { libc::readlinkat(node.fd.as_raw_fd(), empty.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    buf.truncate(n as usize);
    String::from_utf8(buf).map_err(|_| fs_err(errno::EIO, "non-utf8 symlink target"))
  }

  pub(crate) fn link(&self, ino: u64, new_parent: u64, new_name: FileName<'_>) -> Result<v1::FsAttr, v1::FsError> {
    let node = self.get_inode(ino)?;
    let dir = self.get_inode(new_parent)?;
    let c_name = name_to_cstring(new_name)?;
    let proc_path = proc_fd_path(node.fd.as_raw_fd());
    let rc = unsafe { libc::linkat(libc::AT_FDCWD, proc_path.as_ptr(), dir.fd.as_raw_fd(), c_name.as_ptr(), libc::AT_SYMLINK_FOLLOW) };
    if rc < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    self.lookup(new_parent, new_name)
  }

  pub(crate) fn statfs(&self, ino: u64) -> Result<v1::FsStatFsReply, v1::FsError> {
    let node = self.get_inode(ino)?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::fstatvfs(node.fd.as_raw_fd(), &mut st) };
    if rc < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    Ok(v1::FsStatFsReply {
      blocks: st.f_blocks,
      bfree: st.f_bfree,
      bavail: st.f_bavail,
      files: st.f_files,
      ffree: st.f_ffree,
      bsize: st.f_bsize as u32,
      namelen: st.f_namemax as u32,
      frsize: st.f_frsize as u32,
    })
  }

  pub(crate) fn forget(&self, items: &[v1::FsForgetItem]) {
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
      if evict
        && let Some(removed) = store.by_ino.remove(&item.ino)
        && store.by_key.get(&removed.dev_ino) == Some(&item.ino)
      {
        store.by_key.remove(&removed.dev_ino);
      }
    }
  }

  pub(crate) fn getlock(&self, _ino: u64, fh: u64, _lock_owner: u64, range: v1::FsLockRange) -> Result<v1::FsLockRange, v1::FsError> {
    let handle = self.get_handle(fh)?;
    let mut fl = range_to_flock(&range)?;
    let rc = unsafe { libc::fcntl(handle.fd.as_raw_fd(), F_OFD_GETLK, &mut fl) };
    if rc < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    Ok(flock_to_range(&fl, &range))
  }

  pub(crate) fn setlock(&self, _ino: u64, fh: u64, _lock_owner: u64, range: v1::FsLockRange, sleep: bool) -> Result<(), v1::FsError> {
    let handle = self.get_handle(fh)?;
    let fl = range_to_flock(&range)?;
    let cmd = if sleep { F_OFD_SETLKW } else { F_OFD_SETLK };
    let rc = unsafe { libc::fcntl(handle.fd.as_raw_fd(), cmd, &fl) };
    if rc < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    Ok(())
  }
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

fn openat2_in_root(dirfd: RawFd, name: &CStr, flags: u64, mode: u64) -> Result<OwnedFd, v1::FsError> {
  let how = OpenHow {
    flags,
    mode,
    resolve: RESOLVE_IN_ROOT | RESOLVE_NO_MAGICLINKS,
  };
  let raw =
    unsafe { libc::syscall(libc::SYS_openat2, dirfd, name.as_ptr(), &how as *const OpenHow, std::mem::size_of::<OpenHow>()) as libc::c_int };
  if raw >= 0 {
    return Ok(unsafe { OwnedFd::from_raw_fd(raw) });
  }
  let err = io::Error::last_os_error();
  if err.raw_os_error() == Some(libc::ENOSYS) {
    let fallback = unsafe { libc::openat(dirfd, name.as_ptr(), flags as libc::c_int, mode as libc::c_uint) };
    if fallback < 0 {
      return Err(io_to_fs_err(io::Error::last_os_error()));
    }
    return Ok(unsafe { OwnedFd::from_raw_fd(fallback) });
  }
  Err(io_to_fs_err(err))
}

fn proc_fd_path(fd: RawFd) -> CString {
  CString::new(format!("/proc/self/fd/{fd}")).expect("fd decimal path contains no NUL")
}

fn reopen_opath(opath_fd: RawFd, flags: libc::c_int) -> Result<OwnedFd, v1::FsError> {
  // NOTICE: `/proc/self/fd/<n>` is a magic symlink, so `O_NOFOLLOW` fails with
  // `ELOOP` (hit by SQLite's `O_RDWR|O_NOFOLLOW`). `O_CREAT`/`O_EXCL`/`O_NOCTTY`
  // are also meaningless when reopening an existing `O_PATH` inode fd.
  let sanitized = flags & !(libc::O_NOFOLLOW | libc::O_CREAT | libc::O_EXCL | libc::O_NOCTTY);
  let path = proc_fd_path(opath_fd);
  let raw = unsafe { libc::open(path.as_ptr(), sanitized) };
  if raw < 0 {
    return Err(io_to_fs_err(io::Error::last_os_error()));
  }
  Ok(unsafe { OwnedFd::from_raw_fd(raw) })
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
    l_type: l_type as libc::c_short,
    l_whence: libc::SEEK_SET as libc::c_short,
    l_start: range.start.min(i64::MAX as u64) as libc::off_t,
    l_len,
    // `F_OFD_*` locks require `l_pid == 0`.
    l_pid: 0,
  })
}

fn flock_to_range(fl: &libc::flock, requested: &v1::FsLockRange) -> v1::FsLockRange {
  if fl.l_type as libc::c_int == libc::F_UNLCK {
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
  let kind = if fl.l_type as libc::c_int == libc::F_WRLCK {
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

#[allow(dead_code)]
const _: u32 = linux_flags::O_RDONLY;
