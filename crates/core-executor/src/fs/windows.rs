//! Windows executor filesystem server (`WindowsFs`):
//! - Opens handles with `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`.
//! - Implements `Unlink` via `SetFileInformationByHandle(FileDispositionInfoEx)`
//!   with `FILE_DISPOSITION_FLAG_POSIX_SEMANTICS |
//!   FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE`.
//! - Implements `Rename` via `SetFileInformationByHandle(FileRenameInfoEx)`
//!   with `FILE_RENAME_FLAG_POSIX_SEMANTICS | FILE_RENAME_FLAG_REPLACE_IF_EXISTS`.
//! - Maps real NTFS `FileId` (`GetFileInformationByHandleEx(FileIdInfo)`) to
//!   stable FUSE inodes and canonicalizes case via `GetFinalPathNameByHandleW`.
//! - Uses an in-process `AdvisoryLockTable` (never blocking host readers with
//!   mandatory Win32 `LockFileEx`).

use std::collections::HashMap;
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use needy_api_proto::v1;
use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
  CREATE_ALWAYS, CREATE_NEW, CreateDirectoryW, CreateFileW, CreateHardLinkW, CreateSymbolicLinkW, DELETE, FILE_ATTRIBUTE_DIRECTORY,
  FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO, FILE_DISPOSITION_FLAG_DELETE,
  FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX,
  FILE_END_OF_FILE_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES,
  FILE_RENAME_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO, FILE_WRITE_ATTRIBUTES, FileBasicInfo,
  FileDispositionInfo, FileDispositionInfoEx, FileEndOfFileInfo, FileIdInfo, FileRenameInfoEx, FileStandardInfo, FlushFileBuffers,
  GetDiskFreeSpaceExW, GetFileInformationByHandleEx, GetFinalPathNameByHandleW, OPEN_ALWAYS, OPEN_EXISTING, ReadFile, RemoveDirectoryW,
  SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE, SYMBOLIC_LINK_FLAG_DIRECTORY, SetFileInformationByHandle, TRUNCATE_EXISTING, WriteFile,
};
use windows_sys::Win32::System::IO::OVERLAPPED;

use super::{AdvisoryLockTable, FileName, ROOT_INO, adjust_writeback_flags, errno, fs_err, io_to_fs_err, linux_flags, win32_to_linux_errno};

const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

/// `FILE_RENAME_INFO::Flags` bits from Windows SDK `<winbase.h>` (not yet
/// exposed in `windows-sys` 0.59 `Win32::Storage::FileSystem`).
///
/// Reference: Microsoft Docs `FILE_RENAME_INFO` structure (`winbase.h`)
const FILE_RENAME_FLAG_REPLACE_IF_EXISTS: u32 = 0x0000_0001;
const FILE_RENAME_FLAG_POSIX_SEMANTICS: u32 = 0x0000_0002;

/// Windows epoch (1601-01-01) to Unix epoch (1970-01-01) in 100-ns ticks.
///
/// Reference: Microsoft Docs `FILETIME` structure
const WINDOWS_TO_UNIX_EPOCH_TICKS: i64 = 116_444_736_000_000_000;
const TICKS_PER_SEC: i64 = 10_000_000;

type FileIdKey = (u64, [u8; 16]);

struct OwnedHandle(HANDLE);

// SAFETY: Win32 file handles are kernel objects safe to move/share across threads
// when accessed with positional `OVERLAPPED` I/O or synchronized syscalls.
unsafe impl Send for OwnedHandle {}
unsafe impl Sync for OwnedHandle {}

impl OwnedHandle {
  fn raw(&self) -> HANDLE {
    self.0
  }
}

impl Drop for OwnedHandle {
  fn drop(&mut self) {
    if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
      unsafe { CloseHandle(self.0) };
    }
  }
}

struct InodeData {
  key: FileIdKey,
  kind: v1::FsFileType,
  path: Mutex<PathBuf>,
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
  by_key: HashMap<FileIdKey, u64>,
}

struct HandleData {
  handle: OwnedHandle,
}

pub(crate) struct WindowsFs {
  root_attr: v1::FsAttr,
  inodes: RwLock<InodeStore>,
  handles: RwLock<HashMap<u64, Arc<HandleData>>>,
  locks: AdvisoryLockTable,
  next_ino: AtomicU64,
  next_fh: AtomicU64,
}

impl WindowsFs {
  pub(crate) fn open(root: &Path) -> Result<Self, v1::FsError> {
    let handle = open_handle_attrs(root, true)?;
    let (key, attr) = query_handle_id_and_attr(&handle, ROOT_INO)?;
    if attr.kind != v1::FsFileType::Directory as i32 {
      return Err(fs_err(errno::ENOTDIR, "workspace root is not a directory"));
    }
    let canonical = final_path_or_fallback(&handle, root);
    let root_data = Arc::new(InodeData {
      key,
      kind: v1::FsFileType::Directory,
      path: Mutex::new(canonical),
      refcount: AtomicU64::new(1),
    });

    let mut by_ino = HashMap::new();
    let mut by_key = HashMap::new();
    by_ino.insert(ROOT_INO, root_data);
    by_key.insert(key, ROOT_INO);

    Ok(Self {
      root_attr: attr,
      inodes: RwLock::new(InodeStore { by_ino, by_key }),
      handles: RwLock::new(HashMap::new()),
      locks: AdvisoryLockTable::new(),
      next_ino: AtomicU64::new(ROOT_INO + 1),
      next_fh: AtomicU64::new(1),
    })
  }

  pub(crate) fn root_attr(&self) -> v1::FsAttr {
    self.root_attr
  }

  pub(crate) fn case_insensitive(&self) -> bool {
    true
  }

  fn get_inode(&self, ino: u64) -> Result<Arc<InodeData>, v1::FsError> {
    let store = self.inodes.read().unwrap_or_else(|e| e.into_inner());
    store.by_ino.get(&ino).cloned().ok_or_else(|| fs_err(errno::ENOENT, format!("unknown inode {ino}")))
  }

  fn get_handle(&self, fh: u64) -> Result<Arc<HandleData>, v1::FsError> {
    let handles = self.handles.read().unwrap_or_else(|e| e.into_inner());
    handles.get(&fh).cloned().ok_or_else(|| fs_err(errno::EBADF, format!("unknown file handle {fh}")))
  }

  fn insert_handle(&self, handle: OwnedHandle) -> u64 {
    let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
    let mut handles = self.handles.write().unwrap_or_else(|e| e.into_inner());
    handles.insert(fh, Arc::new(HandleData { handle }));
    fh
  }

  fn pin_path(&self, path: &Path) -> Result<v1::FsAttr, v1::FsError> {
    let handle = open_handle_attrs(path, true)?;
    let (key, mut attr) = query_handle_id_and_attr(&handle, 0)?;
    let canonical = final_path_or_fallback(&handle, path);
    let kind = v1::FsFileType::try_from(attr.kind).unwrap_or(v1::FsFileType::Regular);

    {
      let store = self.inodes.read().unwrap_or_else(|e| e.into_inner());
      if let Some(&ino) = store.by_key.get(&key)
        && let Some(existing) = store.by_ino.get(&ino)
        && existing.try_inc_ref()
      {
        *existing.path.lock().unwrap_or_else(|e| e.into_inner()) = canonical;
        attr.ino = ino;
        return Ok(attr);
      }
    }

    let mut store = self.inodes.write().unwrap_or_else(|e| e.into_inner());
    if let Some(&ino) = store.by_key.get(&key)
      && let Some(existing) = store.by_ino.get(&ino)
      && existing.try_inc_ref()
    {
      *existing.path.lock().unwrap_or_else(|e| e.into_inner()) = canonical;
      attr.ino = ino;
      return Ok(attr);
    }

    let ino = self.next_ino.fetch_add(1, Ordering::Relaxed);
    attr.ino = ino;
    let data = Arc::new(InodeData {
      key,
      kind,
      path: Mutex::new(canonical),
      refcount: AtomicU64::new(1),
    });
    store.by_ino.insert(ino, data);
    store.by_key.insert(key, ino);
    Ok(attr)
  }

  fn child_path(&self, parent: u64, name: FileName<'_>) -> Result<PathBuf, v1::FsError> {
    let dir = self.get_inode(parent)?;
    if dir.kind != v1::FsFileType::Directory {
      return Err(fs_err(errno::ENOTDIR, "parent is not a directory"));
    }
    let base = dir.path.lock().unwrap_or_else(|e| e.into_inner()).clone();
    Ok(base.join(name.as_str()))
  }

  pub(crate) fn lookup(&self, parent: u64, name: FileName<'_>) -> Result<v1::FsAttr, v1::FsError> {
    let child = self.child_path(parent, name)?;
    self.pin_path(&child)
  }

  pub(crate) fn getattr(&self, ino: u64, fh: Option<u64>) -> Result<v1::FsAttr, v1::FsError> {
    if let Some(fh) = fh {
      let h = self.get_handle(fh)?;
      let (_, attr) = query_handle_id_and_attr(&h.handle, ino)?;
      return Ok(attr);
    }
    let node = self.get_inode(ino)?;
    let path = node.path.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let h = open_handle_attrs(&path, true)?;
    let (_, attr) = query_handle_id_and_attr(&h, ino)?;
    Ok(attr)
  }

  pub(crate) fn setattr(&self, req: &v1::FsSetAttr) -> Result<v1::FsAttr, v1::FsError> {
    let handle = match req.fh {
      Some(fh) => Some(self.get_handle(fh)?),
      None => None,
    };
    let node = self.get_inode(req.ino)?;
    let path = node.path.lock().unwrap_or_else(|e| e.into_inner()).clone();

    let owned_tmp = if handle.is_none() {
      let mut access = FILE_WRITE_ATTRIBUTES;
      if req.size.is_some() {
        access |= GENERIC_WRITE;
      }
      Some(open_win32(&path, access, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)?)
    } else {
      None
    };

    let raw_handle = if let Some(ref h) = handle {
      h.handle.raw()
    } else {
      owned_tmp.as_ref().unwrap().raw()
    };

    if let Some(size) = req.size {
      let info = FILE_END_OF_FILE_INFO {
        EndOfFile: size as i64,
      };
      let ok = unsafe {
        SetFileInformationByHandle(
          raw_handle,
          FileEndOfFileInfo,
          (&info as *const FILE_END_OF_FILE_INFO).cast(),
          std::mem::size_of::<FILE_END_OF_FILE_INFO>() as u32,
        )
      };
      if ok == 0 {
        return Err(last_win32_err());
      }
    }

    if req.mode.is_some() || req.atime_sec.is_some() || req.mtime_sec.is_some() {
      let mut basic: FILE_BASIC_INFO = unsafe { std::mem::zeroed() };
      let ok = unsafe {
        GetFileInformationByHandleEx(
          raw_handle,
          FileBasicInfo,
          (&mut basic as *mut FILE_BASIC_INFO).cast(),
          std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
      };
      if ok == 0 {
        return Err(last_win32_err());
      }
      if let Some(mode) = req.mode {
        if (mode & 0o222) == 0 {
          basic.FileAttributes |= FILE_ATTRIBUTE_READONLY;
        } else {
          basic.FileAttributes &= !FILE_ATTRIBUTE_READONLY;
          if basic.FileAttributes == 0 {
            basic.FileAttributes = FILE_ATTRIBUTE_NORMAL;
          }
        }
      }
      if let Some(sec) = req.atime_sec {
        basic.LastAccessTime = unix_to_windows_ticks(sec, req.atime_nsec.unwrap_or(0));
      }
      if let Some(sec) = req.mtime_sec {
        basic.LastWriteTime = unix_to_windows_ticks(sec, req.mtime_nsec.unwrap_or(0));
      }
      let ok = unsafe {
        SetFileInformationByHandle(
          raw_handle,
          FileBasicInfo,
          (&basic as *const FILE_BASIC_INFO).cast(),
          std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
      };
      if ok == 0 {
        return Err(last_win32_err());
      }
    }

    drop(owned_tmp);
    self.getattr(req.ino, req.fh)
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
    let dir_path = dir.path.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let rd = std::fs::read_dir(&dir_path).map_err(io_to_fs_err)?;
    let mut names = Vec::new();
    for entry in rd {
      let Ok(ent) = entry else { continue };
      let Ok(name) = ent.file_name().into_string() else {
        continue;
      };
      names.push(name);
    }
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
        Ok(attr) => entries.push(v1::FsDirEntry {
          ino: attr.ino,
          offset: idx as u64,
          name: name.clone(),
          attr: Some(attr),
          generation,
        }),
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
    let path = node.path.lock().unwrap_or_else(|e| e.into_inner()).clone();

    let access = match adjusted & linux_flags::O_ACCMODE {
      linux_flags::O_WRONLY => GENERIC_WRITE | FILE_READ_ATTRIBUTES,
      linux_flags::O_RDWR => GENERIC_READ | GENERIC_WRITE,
      _ => GENERIC_READ,
    };
    let disp = if (adjusted & linux_flags::O_TRUNC) != 0 {
      TRUNCATE_EXISTING
    } else {
      OPEN_EXISTING
    };
    let extra = if node.kind == v1::FsFileType::Directory {
      FILE_FLAG_BACKUP_SEMANTICS
    } else {
      FILE_ATTRIBUTE_NORMAL
    };
    let handle = open_win32(&path, access, disp, extra)?;
    let fh = self.insert_handle(handle);
    Ok((fh, adjusted))
  }

  pub(crate) fn read(&self, _ino: u64, fh: u64, offset: u64, size: usize) -> Result<(Vec<u8>, bool), v1::FsError> {
    let h = self.get_handle(fh)?;
    if size == 0 {
      return Ok((Vec::new(), false));
    }
    let mut buf = vec![0u8; size];
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    overlapped.Anonymous.Anonymous.Offset = offset as u32;
    overlapped.Anonymous.Anonymous.OffsetHigh = (offset >> 32) as u32;
    let mut read_bytes: u32 = 0;
    let ok = unsafe { ReadFile(h.handle.raw(), buf.as_mut_ptr(), size.min(u32::MAX as usize) as u32, &mut read_bytes, &mut overlapped) };
    if ok == 0 {
      let code = unsafe { GetLastError() };
      if code == windows_sys::Win32::Foundation::ERROR_HANDLE_EOF {
        return Ok((Vec::new(), true));
      }
      return Err(fs_err(win32_to_linux_errno(code), format!("ReadFile failed with win32 error {code}")));
    }
    let n = read_bytes as usize;
    buf.truncate(n);
    Ok((buf, n < size))
  }

  pub(crate) fn write(&self, _ino: u64, fh: u64, offset: u64, data: &[u8]) -> Result<u32, v1::FsError> {
    let h = self.get_handle(fh)?;
    let written = write_all_at(&h.handle, data, offset)?;
    Ok(written as u32)
  }

  pub(crate) fn flush(&self, _ino: u64, fh: u64, _lock_owner: u64) -> Result<(), v1::FsError> {
    let _ = self.get_handle(fh)?;
    Ok(())
  }

  pub(crate) fn fsync(&self, _ino: u64, fh: u64, _datasync: bool) -> Result<(), v1::FsError> {
    let h = self.get_handle(fh)?;
    let ok = unsafe { FlushFileBuffers(h.handle.raw()) };
    if ok == 0 {
      return Err(last_win32_err());
    }
    Ok(())
  }

  pub(crate) fn release(&self, ino: u64, fh: u64, lock_owner: u64) -> Result<(), v1::FsError> {
    {
      let mut handles = self.handles.write().unwrap_or_else(|e| e.into_inner());
      handles.remove(&fh);
    }
    if lock_owner != 0 {
      self.locks.release_owner(ino, lock_owner);
    }
    Ok(())
  }

  pub(crate) fn create(
    &self,
    parent: u64,
    name: FileName<'_>,
    _mode: u32,
    flags: u32,
    writeback: bool,
  ) -> Result<(v1::FsAttr, u64, u32), v1::FsError> {
    let path = self.child_path(parent, name)?;
    let adjusted = adjust_writeback_flags(flags, writeback);
    let access = match adjusted & linux_flags::O_ACCMODE {
      linux_flags::O_RDONLY => GENERIC_READ | GENERIC_WRITE,
      linux_flags::O_WRONLY => GENERIC_WRITE | FILE_READ_ATTRIBUTES,
      _ => GENERIC_READ | GENERIC_WRITE,
    };
    let disp = if (adjusted & linux_flags::O_EXCL) != 0 {
      CREATE_NEW
    } else if (adjusted & linux_flags::O_TRUNC) != 0 {
      CREATE_ALWAYS
    } else {
      OPEN_ALWAYS
    };
    let handle = open_win32(&path, access, disp, FILE_ATTRIBUTE_NORMAL)?;
    let attr = self.pin_path(&path)?;
    let fh = self.insert_handle(handle);
    Ok((attr, fh, adjusted))
  }

  pub(crate) fn create_and_write(&self, parent: u64, name: FileName<'_>, _mode: u32, data: &[u8]) -> Result<v1::FsAttr, v1::FsError> {
    let path = self.child_path(parent, name)?;
    let handle = open_win32(&path, GENERIC_WRITE | FILE_READ_ATTRIBUTES, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL)?;
    write_all_at(&handle, data, 0)?;
    drop(handle);
    self.pin_path(&path)
  }

  pub(crate) fn mkdir(&self, parent: u64, name: FileName<'_>, _mode: u32) -> Result<v1::FsAttr, v1::FsError> {
    let path = self.child_path(parent, name)?;
    let wide = to_wide(&path);
    let ok = unsafe { CreateDirectoryW(wide.as_ptr(), std::ptr::null()) };
    if ok == 0 {
      return Err(last_win32_err());
    }
    self.pin_path(&path)
  }

  pub(crate) fn unlink(&self, parent: u64, name: FileName<'_>) -> Result<(), v1::FsError> {
    let path = self.child_path(parent, name)?;
    let handle = open_win32(&path, DELETE | FILE_READ_ATTRIBUTES, OPEN_EXISTING, FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)?;
    let info_ex = FILE_DISPOSITION_INFO_EX {
      Flags: FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    };
    let ok = unsafe {
      SetFileInformationByHandle(
        handle.raw(),
        FileDispositionInfoEx,
        (&info_ex as *const FILE_DISPOSITION_INFO_EX).cast(),
        std::mem::size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
      )
    };
    if ok == 0 {
      let info = FILE_DISPOSITION_INFO { DeleteFile: 1 };
      let fallback = unsafe {
        SetFileInformationByHandle(
          handle.raw(),
          FileDispositionInfo,
          (&info as *const FILE_DISPOSITION_INFO).cast(),
          std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
      };
      if fallback == 0 {
        return Err(last_win32_err());
      }
    }
    Ok(())
  }

  pub(crate) fn rmdir(&self, parent: u64, name: FileName<'_>) -> Result<(), v1::FsError> {
    let path = self.child_path(parent, name)?;
    let wide = to_wide(&path);
    let ok = unsafe { RemoveDirectoryW(wide.as_ptr()) };
    if ok == 0 {
      return Err(last_win32_err());
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
    if (flags & linux_flags::RENAME_EXCHANGE) != 0 {
      return Err(fs_err(errno::EOPNOTSUPP, "RENAME_EXCHANGE not supported on Windows"));
    }
    let old_path = self.child_path(parent, name)?;
    let new_path = self.child_path(new_parent, new_name)?;

    let handle =
      open_win32(&old_path, DELETE | FILE_READ_ATTRIBUTES, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)?;
    let src_key = query_handle_id_and_attr(&handle, 0).ok().map(|(k, _)| k);

    let mut dst_wide: Vec<u16> = new_path.as_os_str().encode_wide().collect();
    let name_bytes = (dst_wide.len() * 2) as u32;
    dst_wide.push(0);

    let header_size = std::mem::size_of::<FILE_RENAME_INFO>();
    let total_size = header_size + dst_wide.len() * 2;
    let mut buf = vec![0u8; total_size];

    let rename_flags = if (flags & linux_flags::RENAME_NOREPLACE) != 0 {
      FILE_RENAME_FLAG_POSIX_SEMANTICS
    } else {
      FILE_RENAME_FLAG_POSIX_SEMANTICS | FILE_RENAME_FLAG_REPLACE_IF_EXISTS
    };

    unsafe {
      let info = buf.as_mut_ptr().cast::<FILE_RENAME_INFO>();
      (*info).Anonymous.Flags = rename_flags;
      (*info).RootDirectory = std::ptr::null_mut();
      (*info).FileNameLength = name_bytes;
      let dst_ptr = std::ptr::addr_of_mut!((*info).FileName).cast::<u16>();
      std::ptr::copy_nonoverlapping(dst_wide.as_ptr(), dst_ptr, dst_wide.len());
    }

    let ok = unsafe { SetFileInformationByHandle(handle.raw(), FileRenameInfoEx, buf.as_ptr().cast(), total_size as u32) };
    if ok == 0 {
      return Err(last_win32_err());
    }

    if let Some(key) = src_key {
      let store = self.inodes.read().unwrap_or_else(|e| e.into_inner());
      if let Some(&ino) = store.by_key.get(&key)
        && let Some(node) = store.by_ino.get(&ino)
      {
        *node.path.lock().unwrap_or_else(|e| e.into_inner()) = new_path;
      }
    }
    Ok(())
  }

  pub(crate) fn symlink(&self, parent: u64, name: FileName<'_>, link_target: &str) -> Result<v1::FsAttr, v1::FsError> {
    let path = self.child_path(parent, name)?;
    let link_wide = to_wide(&path);
    let target_wide = to_wide(Path::new(link_target));
    let target_is_dir = path.parent().map(|p| p.join(link_target).is_dir()).unwrap_or(false);
    let mut flags = SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE;
    if target_is_dir {
      flags |= SYMBOLIC_LINK_FLAG_DIRECTORY;
    }
    let ok = unsafe { CreateSymbolicLinkW(link_wide.as_ptr(), target_wide.as_ptr(), flags) };
    if ok == 0 {
      return Err(last_win32_err());
    }
    self.pin_path(&path)
  }

  pub(crate) fn readlink(&self, ino: u64) -> Result<String, v1::FsError> {
    let node = self.get_inode(ino)?;
    let path = node.path.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let target = std::fs::read_link(&path).map_err(io_to_fs_err)?;
    target.into_os_string().into_string().map_err(|_| fs_err(errno::EIO, "non-utf8 symlink target"))
  }

  pub(crate) fn link(&self, ino: u64, new_parent: u64, new_name: FileName<'_>) -> Result<v1::FsAttr, v1::FsError> {
    let node = self.get_inode(ino)?;
    let existing = node.path.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let new_path = self.child_path(new_parent, new_name)?;
    let existing_w = to_wide(&existing);
    let new_w = to_wide(&new_path);
    let ok = unsafe { CreateHardLinkW(new_w.as_ptr(), existing_w.as_ptr(), std::ptr::null_mut()) };
    if ok == 0 {
      return Err(last_win32_err());
    }
    self.pin_path(&new_path)
  }

  pub(crate) fn statfs(&self, ino: u64) -> Result<v1::FsStatFsReply, v1::FsError> {
    let node = self.get_inode(ino)?;
    let path = node.path.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let wide = to_wide(&path);
    let mut avail: u64 = 0;
    let mut total: u64 = 0;
    let mut free: u64 = 0;
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, &mut total, &mut free) };
    if ok == 0 {
      return Err(last_win32_err());
    }
    let bsize: u32 = 4096;
    Ok(v1::FsStatFsReply {
      blocks: total / bsize as u64,
      bfree: free / bsize as u64,
      bavail: avail / bsize as u64,
      files: 0,
      ffree: 0,
      bsize,
      namelen: 255,
      frsize: bsize,
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
        && store.by_key.get(&removed.key) == Some(&item.ino)
      {
        store.by_key.remove(&removed.key);
      }
    }
  }

  pub(crate) fn getlock(&self, ino: u64, fh: u64, lock_owner: u64, range: v1::FsLockRange) -> Result<v1::FsLockRange, v1::FsError> {
    let _ = self.get_handle(fh)?;
    let owner = if lock_owner != 0 { lock_owner } else { fh };
    self.locks.get_lock(ino, owner, range)
  }

  pub(crate) fn setlock(&self, ino: u64, fh: u64, lock_owner: u64, range: v1::FsLockRange, _sleep: bool) -> Result<(), v1::FsError> {
    let _ = self.get_handle(fh)?;
    let owner = if lock_owner != 0 { lock_owner } else { fh };
    self.locks.set_lock(ino, owner, range)
  }
}

fn to_wide(path: &Path) -> Vec<u16> {
  path.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

fn last_win32_err() -> v1::FsError {
  let code = unsafe { GetLastError() };
  fs_err(win32_to_linux_errno(code), format!("win32 error {code}"))
}

fn open_win32(path: &Path, access: u32, disposition: u32, flags: u32) -> Result<OwnedHandle, v1::FsError> {
  let wide = to_wide(path);
  let handle = unsafe { CreateFileW(wide.as_ptr(), access, SHARE_ALL, std::ptr::null(), disposition, flags, std::ptr::null_mut()) };
  if handle == INVALID_HANDLE_VALUE || handle.is_null() {
    return Err(last_win32_err());
  }
  Ok(OwnedHandle(handle))
}

fn open_handle_attrs(path: &Path, nofollow: bool) -> Result<OwnedHandle, v1::FsError> {
  let mut flags = FILE_FLAG_BACKUP_SEMANTICS;
  if nofollow {
    flags |= FILE_FLAG_OPEN_REPARSE_POINT;
  }
  open_win32(path, FILE_READ_ATTRIBUTES, OPEN_EXISTING, flags)
}

fn query_handle_id_and_attr(handle: &OwnedHandle, ino: u64) -> Result<(FileIdKey, v1::FsAttr), v1::FsError> {
  let mut id_info: FILE_ID_INFO = unsafe { std::mem::zeroed() };
  let ok = unsafe {
    GetFileInformationByHandleEx(
      handle.raw(),
      FileIdInfo,
      (&mut id_info as *mut FILE_ID_INFO).cast(),
      std::mem::size_of::<FILE_ID_INFO>() as u32,
    )
  };
  if ok == 0 {
    return Err(last_win32_err());
  }
  let mut basic: FILE_BASIC_INFO = unsafe { std::mem::zeroed() };
  let ok = unsafe {
    GetFileInformationByHandleEx(
      handle.raw(),
      FileBasicInfo,
      (&mut basic as *mut FILE_BASIC_INFO).cast(),
      std::mem::size_of::<FILE_BASIC_INFO>() as u32,
    )
  };
  if ok == 0 {
    return Err(last_win32_err());
  }
  let mut standard: FILE_STANDARD_INFO = unsafe { std::mem::zeroed() };
  let ok = unsafe {
    GetFileInformationByHandleEx(
      handle.raw(),
      FileStandardInfo,
      (&mut standard as *mut FILE_STANDARD_INFO).cast(),
      std::mem::size_of::<FILE_STANDARD_INFO>() as u32,
    )
  };
  if ok == 0 {
    return Err(last_win32_err());
  }

  let attrs = basic.FileAttributes;
  let kind = if (attrs & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
    v1::FsFileType::Symlink
  } else if (attrs & FILE_ATTRIBUTE_DIRECTORY) != 0 {
    v1::FsFileType::Directory
  } else {
    v1::FsFileType::Regular
  };

  let perm = match kind {
    v1::FsFileType::Directory => 0o755,
    v1::FsFileType::Symlink => 0o777,
    _ => {
      if (attrs & FILE_ATTRIBUTE_READONLY) != 0 {
        0o444
      } else {
        0o644
      }
    }
  };

  let (atime_sec, atime_nsec) = windows_ticks_to_unix(basic.LastAccessTime);
  let (mtime_sec, mtime_nsec) = windows_ticks_to_unix(basic.LastWriteTime);
  let (ctime_sec, ctime_nsec) = windows_ticks_to_unix(basic.ChangeTime);
  let size = standard.EndOfFile.max(0) as u64;
  let blocks = standard.AllocationSize.max(0) as u64 / 512;

  let key = (id_info.VolumeSerialNumber, id_info.FileId.Identifier);
  let attr = v1::FsAttr {
    ino,
    size,
    blocks,
    atime_sec,
    atime_nsec,
    mtime_sec,
    mtime_nsec,
    ctime_sec,
    ctime_nsec,
    kind: kind as i32,
    perm,
    nlink: standard.NumberOfLinks.max(1),
    uid: 0,
    gid: 0,
    blksize: 4096,
  };
  Ok((key, attr))
}

fn final_path_or_fallback(handle: &OwnedHandle, fallback: &Path) -> PathBuf {
  let len = unsafe { GetFinalPathNameByHandleW(handle.raw(), std::ptr::null_mut(), 0, FILE_NAME_NORMALIZED) };
  if len == 0 {
    return fallback.to_path_buf();
  }
  let mut buf = vec![0u16; len as usize];
  let written = unsafe { GetFinalPathNameByHandleW(handle.raw(), buf.as_mut_ptr(), len, FILE_NAME_NORMALIZED) };
  if written == 0 || written >= len {
    return fallback.to_path_buf();
  }
  buf.truncate(written as usize);
  PathBuf::from(OsString::from_wide(&buf))
}

fn write_all_at(handle: &OwnedHandle, mut data: &[u8], mut offset: u64) -> Result<usize, v1::FsError> {
  let mut total = 0usize;
  while !data.is_empty() {
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    overlapped.Anonymous.Anonymous.Offset = offset as u32;
    overlapped.Anonymous.Anonymous.OffsetHigh = (offset >> 32) as u32;
    let mut written: u32 = 0;
    let chunk = data.len().min(u32::MAX as usize) as u32;
    let ok = unsafe { WriteFile(handle.raw(), data.as_ptr(), chunk, &mut written, &mut overlapped) };
    if ok == 0 {
      return Err(last_win32_err());
    }
    let n = written as usize;
    total += n;
    offset += n as u64;
    data = &data[n..];
  }
  Ok(total)
}

fn windows_ticks_to_unix(ticks: i64) -> (i64, u32) {
  let delta = ticks.saturating_sub(WINDOWS_TO_UNIX_EPOCH_TICKS);
  let sec = delta.div_euclid(TICKS_PER_SEC);
  let rem = delta.rem_euclid(TICKS_PER_SEC);
  (sec, (rem * 100) as u32)
}

fn unix_to_windows_ticks(sec: i64, nsec: u32) -> i64 {
  sec.saturating_mul(TICKS_PER_SEC).saturating_add((nsec / 100) as i64).saturating_add(WINDOWS_TO_UNIX_EPOCH_TICKS)
}

#[allow(dead_code)]
const _: Option<FILETIME> = None;
