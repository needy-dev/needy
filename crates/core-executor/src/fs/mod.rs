//! Executor-side workspace filesystem server (`HostFs`).
//!
//! Serves typed `FsService` (`SessionRequest` / `SessionResponse`) RPCs rooted
//! at a workspace directory on the executor host. Platform backends:
//! - Linux: `virtiofsd`-style `openat2(RESOLVE_IN_ROOT | RESOLVE_NO_MAGICLINKS)`
//!   with `O_PATH` inode pinning and `F_OFD_*` per-handle locks.
//! - macOS / BSD: `openat` + `O_NOFOLLOW` + `(dev, ino)` inode table with
//!   `Forget` refcounting and hybrid in-process + `fcntl` locks.
//! - Windows: Win32 `FILE_SHARE_DELETE`, `POSIX_SEMANTICS` unlink/rename,
//!   NTFS `FileId` inode mapping, and in-process advisory locks.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(all(unix, not(target_os = "linux")))]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(test)]
mod mod_test;

#[cfg(not(target_os = "linux"))]
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
#[cfg(not(target_os = "linux"))]
use std::sync::Mutex;

use futures_util::{Stream, StreamExt};
use needy_api_proto::v1;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

/// Root inode number in FUSE / `FsService`.
pub const ROOT_INO: u64 = 1;

/// Default maximum directory entries returned by `ReadDirPlus` when the client
/// leaves `max_entries == 0`.
pub(crate) const DEFAULT_READDIR_MAX: usize = 256;

/// Upper bound on a single `Read` RPC buffer to avoid unbounded allocations
/// from malformed requests.
pub(crate) const MAX_READ_SIZE: usize = 16 * 1024 * 1024;

/// Standard Linux/POSIX errno values carried on the `FsService` wire protocol.
///
/// Source: https://github.com/torvalds/linux/blob/v6.12/include/uapi/asm-generic/errno-base.h
/// Source: https://github.com/torvalds/linux/blob/v6.12/include/uapi/asm-generic/errno.h
/// Reference: Linux `errno(3)`
#[allow(dead_code)]
pub mod errno {
  pub const EPERM: i32 = 1;
  pub const ENOENT: i32 = 2;
  pub const ESRCH: i32 = 3;
  pub const EINTR: i32 = 4;
  pub const EIO: i32 = 5;
  pub const ENXIO: i32 = 6;
  pub const EBADF: i32 = 9;
  pub const EAGAIN: i32 = 11;
  pub const ENOMEM: i32 = 12;
  pub const EACCES: i32 = 13;
  pub const EFAULT: i32 = 14;
  pub const EBUSY: i32 = 16;
  pub const EEXIST: i32 = 17;
  pub const EXDEV: i32 = 18;
  pub const ENODEV: i32 = 19;
  pub const ENOTDIR: i32 = 20;
  pub const EISDIR: i32 = 21;
  pub const EINVAL: i32 = 22;
  pub const ENFILE: i32 = 23;
  pub const EMFILE: i32 = 24;
  pub const EFBIG: i32 = 27;
  pub const ENOSPC: i32 = 28;
  pub const ESPIPE: i32 = 29;
  pub const EROFS: i32 = 30;
  pub const EMLINK: i32 = 31;
  pub const EPIPE: i32 = 32;
  pub const ERANGE: i32 = 34;
  pub const EDEADLK: i32 = 35;
  pub const ENAMETOOLONG: i32 = 36;
  pub const ENOLCK: i32 = 37;
  pub const ENOSYS: i32 = 38;
  pub const ENOTEMPTY: i32 = 39;
  pub const ELOOP: i32 = 40;
  pub const EOVERFLOW: i32 = 75;
  pub const EOPNOTSUPP: i32 = 95;
  pub const ESTALE: i32 = 116;
}

/// Linux `open(2)` and `renameat2(2)` flag constants carried on the wire.
///
/// Source: https://github.com/torvalds/linux/blob/v6.12/include/uapi/asm-generic/fcntl.h
/// Source: https://github.com/torvalds/linux/blob/v6.12/include/uapi/linux/fs.h
/// Reference: Linux `open(2)`, `renameat2(2)`
#[allow(dead_code)]
pub mod linux_flags {
  pub const O_ACCMODE: u32 = 0o3;
  pub const O_RDONLY: u32 = 0o0;
  pub const O_WRONLY: u32 = 0o1;
  pub const O_RDWR: u32 = 0o2;
  pub const O_CREAT: u32 = 0o100;
  pub const O_EXCL: u32 = 0o200;
  pub const O_TRUNC: u32 = 0o1000;
  pub const O_APPEND: u32 = 0o2000;
  pub const O_NONBLOCK: u32 = 0o4000;
  pub const O_DSYNC: u32 = 0o10000;
  pub const O_SYNC: u32 = 0o4010000;

  pub const RENAME_NOREPLACE: u32 = 1 << 0;
  pub const RENAME_EXCHANGE: u32 = 1 << 1;
}

/// Validated single-component filename (no separators, traversal, or NUL).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileName<'a>(&'a str);

impl<'a> FileName<'a> {
  /// Validate a single path component.
  ///
  /// Rejects empty names, `.`, `..`, `/` and NUL on all platforms; on Windows
  /// also rejects `\`, `:`, `?`, `*`, `<`, `>`, `|`, and `"` with `EINVAL`.
  pub fn parse(name: &'a str) -> Result<Self, v1::FsError> {
    if name.is_empty() || name == "." || name == ".." {
      return Err(fs_err(errno::EINVAL, format!("invalid filename: {name:?}")));
    }
    if name.contains('/') || name.contains('\0') {
      return Err(fs_err(errno::EINVAL, format!("invalid filename: {name:?}")));
    }
    #[cfg(windows)]
    if name.chars().any(|c| matches!(c, '\\' | ':' | '?' | '*' | '<' | '>' | '|' | '"')) {
      return Err(fs_err(errno::EINVAL, format!("invalid windows filename: {name:?}")));
    }
    Ok(Self(name))
  }

  /// Borrow the validated filename string.
  pub fn as_str(&self) -> &'a str {
    self.0
  }
}

/// Construct a wire `FsError`.
pub(crate) fn fs_err(code: i32, message: impl Into<String>) -> v1::FsError {
  v1::FsError {
    errno: code,
    message: message.into(),
  }
}

/// Returns true if the Linux open flags request write access or truncation.
pub(crate) fn open_flags_request_write(flags: u32) -> bool {
  let acc = flags & linux_flags::O_ACCMODE;
  acc == linux_flags::O_WRONLY
    || acc == linux_flags::O_RDWR
    || (flags & (linux_flags::O_TRUNC | linux_flags::O_APPEND | linux_flags::O_CREAT)) != 0
}

/// Apply `FUSE_WRITEBACK_CACHE` open-flag adjustments: promote `O_WRONLY` to
/// `O_RDWR` (the kernel may read back partial pages) and strip `O_APPEND`
/// (the kernel owns writeback offsets).
pub(crate) fn adjust_writeback_flags(flags: u32, writeback: bool) -> u32 {
  if !writeback {
    return flags;
  }
  let mut adjusted = flags & !linux_flags::O_APPEND;
  if (adjusted & linux_flags::O_ACCMODE) == linux_flags::O_WRONLY {
    adjusted = (adjusted & !linux_flags::O_ACCMODE) | linux_flags::O_RDWR;
  }
  adjusted
}

/// Convert a `std::io::Error` into a wire `FsError` with Linux POSIX errno.
pub(crate) fn io_to_fs_err(err: std::io::Error) -> v1::FsError {
  let code = io_error_to_linux_errno(&err);
  v1::FsError {
    errno: code,
    message: err.to_string(),
  }
}

#[cfg(target_os = "linux")]
pub(crate) fn os_errno_to_linux(raw: i32) -> i32 {
  if raw > 0 { raw } else { errno::EIO }
}

#[cfg(all(unix, not(target_os = "linux")))]
pub(crate) fn os_errno_to_linux(raw: i32) -> i32 {
  match raw {
    libc::EPERM => errno::EPERM,
    libc::ENOENT => errno::ENOENT,
    libc::ESRCH => errno::ESRCH,
    libc::EINTR => errno::EINTR,
    libc::EIO => errno::EIO,
    libc::ENXIO => errno::ENXIO,
    libc::EBADF => errno::EBADF,
    libc::EAGAIN => errno::EAGAIN,
    libc::ENOMEM => errno::ENOMEM,
    libc::EACCES => errno::EACCES,
    libc::EFAULT => errno::EFAULT,
    libc::EBUSY => errno::EBUSY,
    libc::EEXIST => errno::EEXIST,
    libc::EXDEV => errno::EXDEV,
    libc::ENODEV => errno::ENODEV,
    libc::ENOTDIR => errno::ENOTDIR,
    libc::EISDIR => errno::EISDIR,
    libc::EINVAL => errno::EINVAL,
    libc::ENFILE => errno::ENFILE,
    libc::EMFILE => errno::EMFILE,
    libc::EFBIG => errno::EFBIG,
    libc::ENOSPC => errno::ENOSPC,
    libc::ESPIPE => errno::ESPIPE,
    libc::EROFS => errno::EROFS,
    libc::EMLINK => errno::EMLINK,
    libc::EPIPE => errno::EPIPE,
    libc::ERANGE => errno::ERANGE,
    libc::EDEADLK => errno::EDEADLK,
    libc::ENAMETOOLONG => errno::ENAMETOOLONG,
    libc::ENOLCK => errno::ENOLCK,
    libc::ENOSYS => errno::ENOSYS,
    libc::ENOTEMPTY => errno::ENOTEMPTY,
    libc::ELOOP => errno::ELOOP,
    libc::EOVERFLOW => errno::EOVERFLOW,
    libc::EOPNOTSUPP | libc::ENOTSUP => errno::EOPNOTSUPP,
    libc::ESTALE => errno::ESTALE,
    _ => errno::EIO,
  }
}

#[cfg(windows)]
pub(crate) fn win32_to_linux_errno(code: u32) -> i32 {
  use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_BAD_NETPATH, ERROR_BROKEN_PIPE, ERROR_CANT_RESOLVE_FILENAME, ERROR_DIR_NOT_EMPTY,
    ERROR_DIRECTORY, ERROR_DISK_FULL, ERROR_FILE_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_FILENAME_EXCED_RANGE, ERROR_HANDLE_DISK_FULL,
    ERROR_INVALID_DRIVE, ERROR_INVALID_FUNCTION, ERROR_INVALID_HANDLE, ERROR_INVALID_NAME, ERROR_INVALID_PARAMETER, ERROR_LOCK_VIOLATION,
    ERROR_NOT_ENOUGH_MEMORY, ERROR_NOT_SAME_DEVICE, ERROR_NOT_SUPPORTED, ERROR_OUTOFMEMORY, ERROR_PATH_NOT_FOUND, ERROR_PRIVILEGE_NOT_HELD,
    ERROR_SHARING_VIOLATION, ERROR_TOO_MANY_OPEN_FILES, ERROR_WRITE_PROTECT,
  };
  match code {
    ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND | ERROR_BAD_NETPATH | ERROR_INVALID_DRIVE => errno::ENOENT,
    ERROR_ACCESS_DENIED => errno::EACCES,
    ERROR_PRIVILEGE_NOT_HELD => errno::EPERM,
    ERROR_INVALID_HANDLE => errno::EBADF,
    ERROR_NOT_ENOUGH_MEMORY | ERROR_OUTOFMEMORY => errno::ENOMEM,
    ERROR_TOO_MANY_OPEN_FILES => errno::EMFILE,
    ERROR_WRITE_PROTECT => errno::EROFS,
    ERROR_NOT_SAME_DEVICE => errno::EXDEV,
    ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION => errno::EAGAIN,
    ERROR_FILE_EXISTS | ERROR_ALREADY_EXISTS => errno::EEXIST,
    ERROR_DIR_NOT_EMPTY => errno::ENOTEMPTY,
    ERROR_DIRECTORY => errno::ENOTDIR,
    ERROR_INVALID_PARAMETER | ERROR_INVALID_NAME => errno::EINVAL,
    ERROR_FILENAME_EXCED_RANGE => errno::ENAMETOOLONG,
    ERROR_DISK_FULL | ERROR_HANDLE_DISK_FULL => errno::ENOSPC,
    ERROR_BROKEN_PIPE => errno::EPIPE,
    ERROR_CANT_RESOLVE_FILENAME => errno::ELOOP,
    ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED => errno::EOPNOTSUPP,
    _ => errno::EIO,
  }
}

fn io_error_to_linux_errno(err: &std::io::Error) -> i32 {
  if let Some(raw) = err.raw_os_error() {
    #[cfg(unix)]
    return os_errno_to_linux(raw);
    #[cfg(windows)]
    if raw >= 0 {
      return win32_to_linux_errno(raw as u32);
    }
  }
  use std::io::ErrorKind;
  match err.kind() {
    ErrorKind::NotFound => errno::ENOENT,
    ErrorKind::PermissionDenied => errno::EACCES,
    ErrorKind::AlreadyExists => errno::EEXIST,
    ErrorKind::WouldBlock => errno::EAGAIN,
    ErrorKind::InvalidInput => errno::EINVAL,
    ErrorKind::Interrupted => errno::EINTR,
    ErrorKind::BrokenPipe => errno::EPIPE,
    ErrorKind::NotADirectory => errno::ENOTDIR,
    ErrorKind::IsADirectory => errno::EISDIR,
    ErrorKind::DirectoryNotEmpty => errno::ENOTEMPTY,
    ErrorKind::ReadOnlyFilesystem => errno::EROFS,
    ErrorKind::Unsupported => errno::EOPNOTSUPP,
    ErrorKind::OutOfMemory => errno::ENOMEM,
    _ => errno::EIO,
  }
}

/// In-process POSIX advisory byte-range lock table.
///
/// Used directly on Windows (avoiding mandatory Win32 `LockFileEx` blocking
/// host readers) and paired with `fcntl` on macOS/BSD (where POSIX `fcntl`
/// locks are process-scoped and do not conflict between two handles owned by
/// the same executor process).
#[cfg(not(target_os = "linux"))]
#[derive(Default)]
pub(crate) struct AdvisoryLockTable {
  locks: Mutex<HashMap<u64, Vec<HeldLock>>>,
}

#[cfg(not(target_os = "linux"))]
#[derive(Debug, Clone, Copy)]
struct HeldLock {
  owner: u64,
  start: u64,
  end: u64,
  exclusive: bool,
  pid: u32,
}

#[cfg(not(target_os = "linux"))]
impl AdvisoryLockTable {
  pub(crate) fn new() -> Self {
    Self::default()
  }

  /// Check if `range` conflicts with any lock held by a different `owner`.
  pub(crate) fn get_lock(&self, ino: u64, owner: u64, range: v1::FsLockRange) -> Result<v1::FsLockRange, v1::FsError> {
    let kind = v1::FsLockType::try_from(range.kind).unwrap_or(v1::FsLockType::Unspecified);
    let want_exclusive = match kind {
      v1::FsLockType::Read => false,
      v1::FsLockType::Write => true,
      v1::FsLockType::Unlock => {
        return Ok(v1::FsLockRange {
          start: range.start,
          end: range.end,
          kind: v1::FsLockType::Unlock as i32,
          pid: 0,
        });
      }
      v1::FsLockType::Unspecified => {
        return Err(fs_err(errno::EINVAL, "unspecified lock type"));
      }
    };
    if range.end < range.start {
      return Err(fs_err(errno::EINVAL, "lock end before start"));
    }

    let guard = self.locks.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(entries) = guard.get(&ino) {
      for held in entries {
        if held.owner == owner {
          continue;
        }
        if ranges_overlap(held.start, held.end, range.start, range.end) && (held.exclusive || want_exclusive) {
          return Ok(v1::FsLockRange {
            start: held.start,
            end: held.end,
            kind: if held.exclusive {
              v1::FsLockType::Write as i32
            } else {
              v1::FsLockType::Read as i32
            },
            pid: held.pid,
          });
        }
      }
    }

    Ok(v1::FsLockRange {
      start: range.start,
      end: range.end,
      kind: v1::FsLockType::Unlock as i32,
      pid: 0,
    })
  }

  /// Acquire or release a byte-range lock for `(ino, owner)`.
  pub(crate) fn set_lock(&self, ino: u64, owner: u64, range: v1::FsLockRange) -> Result<(), v1::FsError> {
    let kind = v1::FsLockType::try_from(range.kind).unwrap_or(v1::FsLockType::Unspecified);
    if range.end < range.start {
      return Err(fs_err(errno::EINVAL, "lock end before start"));
    }

    let mut guard = self.locks.lock().unwrap_or_else(|e| e.into_inner());
    match kind {
      v1::FsLockType::Unspecified => Err(fs_err(errno::EINVAL, "unspecified lock type")),
      v1::FsLockType::Unlock => {
        if let Some(entries) = guard.get_mut(&ino) {
          subtract_owner_range(entries, owner, range.start, range.end);
          if entries.is_empty() {
            guard.remove(&ino);
          }
        }
        Ok(())
      }
      v1::FsLockType::Read | v1::FsLockType::Write => {
        let want_exclusive = kind == v1::FsLockType::Write;
        let entries = guard.entry(ino).or_default();
        for held in entries.iter() {
          if held.owner == owner {
            continue;
          }
          if ranges_overlap(held.start, held.end, range.start, range.end) && (held.exclusive || want_exclusive) {
            return Err(fs_err(errno::EAGAIN, "conflicting byte-range lock"));
          }
        }
        // Replace any existing lock by the same owner on the overlapping span.
        subtract_owner_range(entries, owner, range.start, range.end);
        entries.push(HeldLock {
          owner,
          start: range.start,
          end: range.end,
          exclusive: want_exclusive,
          pid: range.pid,
        });
        Ok(())
      }
    }
  }

  /// Release all locks held by `owner` on `ino`. Returns `true` if no locks
  /// remain on `ino` across all owners.
  pub(crate) fn release_owner(&self, ino: u64, owner: u64) -> bool {
    let mut guard = self.locks.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(entries) = guard.get_mut(&ino) {
      entries.retain(|l| l.owner != owner);
      if entries.is_empty() {
        guard.remove(&ino);
        return true;
      }
      return false;
    }
    true
  }
}

#[cfg(not(target_os = "linux"))]
fn ranges_overlap(a_start: u64, a_end: u64, b_start: u64, b_end: u64) -> bool {
  a_start <= b_end && b_start <= a_end
}

#[cfg(not(target_os = "linux"))]
fn subtract_owner_range(entries: &mut Vec<HeldLock>, owner: u64, start: u64, end: u64) {
  let mut updated = Vec::with_capacity(entries.len());
  for lock in entries.drain(..) {
    if lock.owner != owner || !ranges_overlap(lock.start, lock.end, start, end) {
      updated.push(lock);
      continue;
    }
    if lock.start < start {
      updated.push(HeldLock {
        end: start - 1,
        ..lock
      });
    }
    if lock.end > end {
      updated.push(HeldLock {
        start: end + 1,
        ..lock
      });
    }
  }
  *entries = updated;
}

#[cfg(target_os = "linux")]
type PlatformFs = linux::PassthroughFs;
#[cfg(all(unix, not(target_os = "linux")))]
type PlatformFs = unix::UnixFs;
#[cfg(windows)]
type PlatformFs = windows::WindowsFs;

struct HostFsInner {
  backend: PlatformFs,
  writable: bool,
  lock_mode: v1::FsLockMode,
  generation: u64,
  case_insensitive: bool,
}

/// Executor-side workspace filesystem server.
///
/// Cloneable handle backed by an `Arc`, safe to share across concurrent Tokio
/// tasks handling a multiplexed `FsService` session.
#[derive(Clone)]
pub struct HostFs {
  inner: Arc<HostFsInner>,
}

impl HostFs {
  /// Open a workspace root directory for serving `FsService` RPCs.
  pub fn open(root: impl AsRef<Path>, writable: bool, lock_mode: v1::FsLockMode) -> Result<Self, v1::FsError> {
    let backend = PlatformFs::open(root.as_ref())?;
    let case_insensitive = backend.case_insensitive();
    let effective_lock_mode = match lock_mode {
      v1::FsLockMode::Unspecified => v1::FsLockMode::Local,
      other => other,
    };
    Ok(Self {
      inner: Arc::new(HostFsInner {
        backend,
        writable,
        lock_mode: effective_lock_mode,
        generation: 1,
        case_insensitive,
      }),
    })
  }

  /// Attributes of the mounted workspace root (`ino = 1`).
  pub fn root_attr(&self) -> v1::FsAttr {
    self.inner.backend.root_attr()
  }

  /// True when the backing workspace filesystem is case-insensitive.
  pub fn case_insensitive(&self) -> bool {
    self.inner.case_insensitive
  }

  /// True when mutating operations are permitted on this mount.
  pub fn writable(&self) -> bool {
    self.inner.writable
  }

  /// Negotiated lock mode for this mount.
  pub fn lock_mode(&self) -> v1::FsLockMode {
    self.inner.lock_mode
  }

  /// Generation number reported for entries created in this session.
  pub fn generation(&self) -> u64 {
    self.inner.generation
  }

  fn mounted_reply(&self) -> v1::FsMounted {
    v1::FsMounted {
      root: Some(self.root_attr()),
      generation: self.inner.generation,
      lock_mode: self.inner.lock_mode as i32,
      writable: self.inner.writable,
      case_insensitive: self.inner.case_insensitive,
    }
  }

  /// Handle a single `SessionRequest`, offloading blocking filesystem syscalls
  /// to Tokio's blocking thread pool.
  pub async fn handle(&self, req: v1::SessionRequest) -> v1::SessionResponse {
    let id = req.id;
    let Some(body) = req.body else {
      return err_response(id, fs_err(errno::EINVAL, "empty session request"));
    };

    // Fast path for non-blocking control messages.
    match body {
      v1::session_request::Body::Ping(p) => v1::SessionResponse {
        id,
        body: Some(v1::session_response::Body::Pong(v1::FsPong {
          timestamp_ms: p.timestamp_ms,
        })),
      },
      v1::session_request::Body::Mount(_) => v1::SessionResponse {
        id,
        body: Some(v1::session_response::Body::Mounted(self.mounted_reply())),
      },
      other => {
        let inner = Arc::clone(&self.inner);
        let result = tokio::task::spawn_blocking(move || dispatch_sync(&inner, other)).await;
        match result {
          Ok(Ok(resp_body)) => v1::SessionResponse {
            id,
            body: Some(resp_body),
          },
          Ok(Err(e)) => err_response(id, e),
          Err(join_err) => err_response(id, fs_err(errno::EIO, format!("fs worker failed: {join_err}"))),
        }
      }
    }
  }

  /// Serve a multiplexed `SessionRequest` stream: processes the initial
  /// `FsMount` handshake, then dispatches subsequent requests concurrently
  /// onto Tokio tasks and sends `SessionResponse`s to `outbound`.
  pub async fn serve_stream<In, E, OpenFn>(mut incoming: In, outbound: mpsc::Sender<v1::SessionResponse>, open_fs: OpenFn)
  where
    In: Stream<Item = Result<v1::SessionRequest, E>> + Unpin + Send + 'static,
    E: std::fmt::Display + Send + 'static,
    OpenFn: FnOnce(&v1::FsMount) -> Result<HostFs, v1::FsError> + Send,
  {
    let first = match incoming.next().await {
      Some(Ok(req)) => req,
      Some(Err(err)) => {
        tracing::debug!("fs session stream error before mount: {err}");
        return;
      }
      None => return,
    };

    let mount_req = match first.body {
      Some(v1::session_request::Body::Mount(m)) => m,
      _ => {
        let _ = outbound.send(err_response(first.id, fs_err(errno::EINVAL, "first request on fs session must be FsMount"))).await;
        return;
      }
    };

    let fs = match open_fs(&mount_req) {
      Ok(fs) => fs,
      Err(err) => {
        let _ = outbound.send(err_response(first.id, err)).await;
        return;
      }
    };

    if outbound
      .send(v1::SessionResponse {
        id: first.id,
        body: Some(v1::session_response::Body::Mounted(fs.mounted_reply())),
      })
      .await
      .is_err()
    {
      return;
    }

    let mut tasks = JoinSet::new();
    while let Some(item) = incoming.next().await {
      match item {
        Ok(req) => {
          let fs = fs.clone();
          let tx = outbound.clone();
          tasks.spawn(async move {
            let resp = fs.handle(req).await;
            let _ = tx.send(resp).await;
          });
        }
        Err(err) => {
          tracing::debug!("fs session stream error: {err}");
          break;
        }
      }
      // Reap completed tasks without blocking the receive loop.
      while tasks.try_join_next().is_some() {}
    }

    while tasks.join_next().await.is_some() {}
  }
}

fn err_response(id: u64, err: v1::FsError) -> v1::SessionResponse {
  v1::SessionResponse {
    id,
    body: Some(v1::session_response::Body::Error(err)),
  }
}

fn require_writable(inner: &HostFsInner) -> Result<(), v1::FsError> {
  if !inner.writable {
    return Err(fs_err(errno::EROFS, "read-only workspace mount"));
  }
  Ok(())
}

fn dispatch_sync(inner: &HostFsInner, body: v1::session_request::Body) -> Result<v1::session_response::Body, v1::FsError> {
  use v1::session_request::Body as Req;
  use v1::session_response::Body as Resp;

  let b = &inner.backend;
  let generation = inner.generation;

  match body {
    Req::Mount(_) | Req::Ping(_) => unreachable!("handled on async fast path"),
    Req::Lookup(r) => {
      let name = FileName::parse(&r.name)?;
      let attr = b.lookup(r.parent, name)?;
      Ok(Resp::Entry(v1::FsEntry {
        attr: Some(attr),
        generation,
      }))
    }
    Req::Getattr(r) => {
      let attr = b.getattr(r.ino, r.fh)?;
      Ok(Resp::Attr(v1::FsAttrReply { attr: Some(attr) }))
    }
    Req::Setattr(r) => {
      require_writable(inner)?;
      let attr = b.setattr(&r)?;
      Ok(Resp::Attr(v1::FsAttrReply { attr: Some(attr) }))
    }
    Req::Readdirplus(r) => {
      let max_entries = if r.max_entries == 0 {
        DEFAULT_READDIR_MAX
      } else {
        r.max_entries as usize
      };
      let (entries, eof) = b.readdirplus(r.ino, r.offset, max_entries, generation)?;
      Ok(Resp::Readdirplus(v1::FsReadDirPlusReply { entries, eof }))
    }
    Req::Open(r) => {
      if open_flags_request_write(r.flags) {
        require_writable(inner)?;
      }
      let (fh, open_flags) = b.open_file(r.ino, r.flags, r.writeback)?;
      Ok(Resp::Opened(v1::FsOpened { fh, open_flags }))
    }
    Req::Read(r) => {
      let size = (r.size as usize).min(MAX_READ_SIZE);
      let (data, eof) = b.read(r.ino, r.fh, r.offset, size)?;
      Ok(Resp::Data(v1::FsData { data, eof }))
    }
    Req::Write(r) => {
      require_writable(inner)?;
      let written = b.write(r.ino, r.fh, r.offset, &r.data)?;
      Ok(Resp::Written(v1::FsWritten { written }))
    }
    Req::Flush(r) => {
      b.flush(r.ino, r.fh, r.lock_owner)?;
      Ok(Resp::Ok(v1::FsOk {}))
    }
    Req::Fsync(r) => {
      b.fsync(r.ino, r.fh, r.datasync)?;
      Ok(Resp::Ok(v1::FsOk {}))
    }
    Req::Release(r) => {
      b.release(r.ino, r.fh, r.lock_owner)?;
      Ok(Resp::Ok(v1::FsOk {}))
    }
    Req::Create(r) => {
      require_writable(inner)?;
      let name = FileName::parse(&r.name)?;
      let (attr, fh, open_flags) = b.create(r.parent, name, r.mode, r.flags, r.writeback)?;
      Ok(Resp::Created(v1::FsCreated {
        attr: Some(attr),
        generation,
        fh,
        open_flags,
      }))
    }
    Req::CreateAndWrite(r) => {
      require_writable(inner)?;
      let name = FileName::parse(&r.name)?;
      let attr = b.create_and_write(r.parent, name, r.mode, &r.data)?;
      Ok(Resp::Entry(v1::FsEntry {
        attr: Some(attr),
        generation,
      }))
    }
    Req::Mkdir(r) => {
      require_writable(inner)?;
      let name = FileName::parse(&r.name)?;
      let attr = b.mkdir(r.parent, name, r.mode)?;
      Ok(Resp::Entry(v1::FsEntry {
        attr: Some(attr),
        generation,
      }))
    }
    Req::Unlink(r) => {
      require_writable(inner)?;
      let name = FileName::parse(&r.name)?;
      b.unlink(r.parent, name)?;
      Ok(Resp::Ok(v1::FsOk {}))
    }
    Req::Rmdir(r) => {
      require_writable(inner)?;
      let name = FileName::parse(&r.name)?;
      b.rmdir(r.parent, name)?;
      Ok(Resp::Ok(v1::FsOk {}))
    }
    Req::Rename(r) => {
      require_writable(inner)?;
      let name = FileName::parse(&r.name)?;
      let new_name = FileName::parse(&r.new_name)?;
      b.rename(r.parent, name, r.new_parent, new_name, r.flags)?;
      Ok(Resp::Ok(v1::FsOk {}))
    }
    Req::Symlink(r) => {
      require_writable(inner)?;
      let name = FileName::parse(&r.name)?;
      if r.link_target.contains('\0') {
        return Err(fs_err(errno::EINVAL, "symlink target contains NUL"));
      }
      let attr = b.symlink(r.parent, name, &r.link_target)?;
      Ok(Resp::Entry(v1::FsEntry {
        attr: Some(attr),
        generation,
      }))
    }
    Req::Readlink(r) => {
      let target = b.readlink(r.ino)?;
      Ok(Resp::LinkTarget(v1::FsLinkTarget { target }))
    }
    Req::Link(r) => {
      require_writable(inner)?;
      let new_name = FileName::parse(&r.new_name)?;
      let attr = b.link(r.ino, r.new_parent, new_name)?;
      Ok(Resp::Entry(v1::FsEntry {
        attr: Some(attr),
        generation,
      }))
    }
    Req::Statfs(r) => {
      let reply = b.statfs(r.ino)?;
      Ok(Resp::Statfs(reply))
    }
    Req::Forget(r) => {
      b.forget(&r.items);
      Ok(Resp::Ok(v1::FsOk {}))
    }
    Req::Getlock(r) => {
      let range = r.range.ok_or_else(|| fs_err(errno::EINVAL, "missing lock range"))?;
      let reply = b.getlock(r.ino, r.fh, r.lock_owner, range)?;
      Ok(Resp::Lock(v1::FsLockReply { range: Some(reply) }))
    }
    Req::Setlock(r) => {
      let range = r.range.ok_or_else(|| fs_err(errno::EINVAL, "missing lock range"))?;
      b.setlock(r.ino, r.fh, r.lock_owner, range, r.sleep)?;
      Ok(Resp::Ok(v1::FsOk {}))
    }
    Req::PrefetchTree(r) => {
      const MAX_DEPTH_CAP: usize = 8;
      const MAX_DIRS_CAP: usize = 4096;
      const MAX_TOTAL_ENTRIES: usize = 20_000;
      const MAX_INLINE_FILE_CAP: usize = 64 * 1024;
      const MAX_INLINE_TOTAL_CAP: usize = 8 * 1024 * 1024;

      let max_depth = (r.max_depth as usize).min(MAX_DEPTH_CAP);
      let max_dirs = if r.max_dirs == 0 {
        256
      } else {
        (r.max_dirs as usize).min(MAX_DIRS_CAP)
      };
      let max_inline_file = (r.max_inline_file_bytes as usize).min(MAX_INLINE_FILE_CAP);
      let max_inline_total = (r.max_inline_total_bytes as usize).min(MAX_INLINE_TOTAL_CAP);
      let skip: std::collections::HashSet<&str> = r.skip_names.iter().map(String::as_str).collect();
      let mut queue = std::collections::VecDeque::new();
      queue.push_back((r.ino, 0usize, String::new()));

      let mut dirs = Vec::new();
      let mut files = Vec::new();
      let mut total_entries = 0usize;
      let mut total_inline_bytes = 0usize;
      while let Some((dir_ino, depth, rel_dir)) = queue.pop_front() {
        if dirs.len() >= max_dirs || total_entries >= MAX_TOTAL_ENTRIES {
          break;
        }
        let mut all_entries = Vec::new();
        let mut offset = 0u64;
        let mut failed = false;
        loop {
          match b.readdirplus(dir_ino, offset, DEFAULT_READDIR_MAX, generation) {
            Ok((batch, eof)) => {
              let last_offset = batch.last().map(|e| e.offset);
              all_entries.extend(batch);
              if eof {
                break;
              }
              match last_offset {
                Some(next) if next > offset => offset = next,
                _ => break,
              }
            }
            Err(_) => {
              failed = true;
              break;
            }
          }
        }
        if failed {
          continue;
        }
        let in_git = rel_dir == ".git" || rel_dir.starts_with(".git/");
        let in_agent_meta =
          rel_dir == ".agents" || rel_dir.starts_with(".agents/") || rel_dir == ".claude" || rel_dir.starts_with(".claude/");

        for entry in &all_entries {
          let Some(attr) = entry.attr.as_ref() else {
            continue;
          };
          let kind = v1::FsFileType::try_from(attr.kind).ok();
          if kind == Some(v1::FsFileType::Directory) {
            if depth < max_depth && entry.ino > ROOT_INO && !skip.contains(entry.name.as_str()) {
              let skip_git_sub = in_git && matches!(entry.name.as_str(), "objects" | "logs" | "hooks" | "worktrees" | "modules");
              if !skip_git_sub {
                let child_rel = if rel_dir.is_empty() {
                  entry.name.clone()
                } else {
                  format!("{rel_dir}/{}", entry.name)
                };
                queue.push_back((entry.ino, depth + 1, child_rel));
              }
            }
          } else if kind == Some(v1::FsFileType::Regular) && max_inline_file > 0 {
            let size = attr.size as usize;
            let want_inline = in_agent_meta
              || (in_git && matches!(entry.name.as_str(), "HEAD" | "config" | "packed-refs" | "commondir"))
              || matches!(
                entry.name.as_str(),
                "SKILL.md"
                  | "CLAUDE.md"
                  | "AGENTS.md"
                  | "package.json"
                  | "Cargo.toml"
                  | "tsconfig.json"
                  | ".gitignore"
                  | "settings.json"
                  | "settings.local.json"
              );
            if want_inline
              && size <= max_inline_file
              && total_inline_bytes.saturating_add(size) <= max_inline_total
              && let Ok((fh, _)) = b.open_file(entry.ino, linux_flags::O_RDONLY, false)
            {
              let read_res = b.read(entry.ino, fh, 0, size);
              let _ = b.release(entry.ino, fh, 0);
              if let Ok((data, _)) = read_res {
                total_inline_bytes += data.len();
                files.push(v1::FsPrefetchFileData {
                  ino: entry.ino,
                  data,
                });
              }
            }
          }
        }
        total_entries += all_entries.len();
        dirs.push(v1::FsPrefetchDir {
          ino: dir_ino,
          entries: all_entries,
        });
      }
      Ok(Resp::PrefetchTree(v1::FsPrefetchTreeReply { dirs, files }))
    }
  }
}
