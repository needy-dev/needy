//! Actor-side async FUSE3 workspace mount client over `FsService`.
//!
//! Platform-neutral components ([`client`], [`neg_cache`], [`prefetch`],
//! [`write_behind`]) build and test on every OS. The Linux FUSE3 adapter lives
//! in `fuse` (`#[cfg(target_os = "linux")]`).

pub mod client;
#[cfg(target_os = "linux")]
pub mod fuse;
pub mod neg_cache;
pub mod prefetch;
pub mod write_behind;

#[cfg(test)]
mod mod_test;

use std::time::Duration;

use needy_api_proto::v1::FsLockMode;

#[cfg(target_os = "linux")]
pub use crate::sshfs::Mount;

/// Environment variable selecting the workspace mount backend.
///
/// - Unset / empty / `"auto"`: use `fuse3` (`FsService`) and fall back to
///   `sshfs` if the server returns `UNIMPLEMENTED`.
/// - `"fuse3"`: require `fuse3` (`FsService`) without falling back to `sshfs`.
/// - `"sshfs"`: use `sshfs` directly.
pub const FS_BACKEND_ENV: &str = "NEEDY_FS_BACKEND";

#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FsBackend {
  Auto,
  Fuse3,
  Sshfs,
}

#[cfg(any(target_os = "linux", test))]
impl FsBackend {
  #[cfg(target_os = "linux")]
  fn from_env() -> Self {
    Self::parse(std::env::var(FS_BACKEND_ENV).ok().as_deref())
  }

  pub(crate) fn parse(raw: Option<&str>) -> Self {
    match raw.map(str::trim) {
      Some(v) if v.eq_ignore_ascii_case("sshfs") => Self::Sshfs,
      Some(v) if v.eq_ignore_ascii_case("fuse3") => Self::Fuse3,
      _ => Self::Auto,
    }
  }
}

/// Returns `true` when `err` carries a gRPC `UNIMPLEMENTED` status, indicating
/// an older server that does not serve `FsService`.
#[cfg(all(unix, any(target_os = "linux", test)))]
pub(crate) fn is_unimplemented(err: &anyhow::Error) -> bool {
  err.chain().any(|cause| {
    if let Some(needy_api_client::Error::Status(status)) = cause.downcast_ref::<needy_api_client::Error>() {
      return status.code() == tonic::Code::Unimplemented;
    }
    if let Some(status) = cause.downcast_ref::<tonic::Status>() {
      return status.code() == tonic::Code::Unimplemented;
    }
    false
  })
}

/// Configuration for mounting an executor workspace via `FsService`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountConfig {
  /// Mount target path on this machine.
  pub target: String,
  /// Executor name in the run's profile.
  pub executor: String,
  /// Workspace root path on the executor.
  pub source: String,
  /// Whether the mount permits writes.
  pub writable: bool,
  /// POSIX file lock enforcement mode (`Local` or `Remote`).
  pub lock_mode: FsLockMode,
  /// Owner UID reported for mounted files and applied to newly created mount dirs.
  pub uid: u32,
  /// Owner GID reported for mounted files and applied to newly created mount dirs.
  pub gid: u32,
  /// Kernel attribute cache TTL.
  pub attr_ttl: Duration,
  /// Kernel directory entry cache TTL.
  pub entry_ttl: Duration,
  /// Whether the remote executor filesystem is case-insensitive (set from `FsMounted`).
  pub case_insensitive: bool,
}

impl MountConfig {
  /// Default kernel attribute cache TTL.
  pub const DEFAULT_ATTR_TTL: Duration = Duration::from_secs(1);
  /// Default kernel directory entry cache TTL.
  pub const DEFAULT_ENTRY_TTL: Duration = Duration::from_secs(1);

  /// Creates a `MountConfig` with default 1-second attribute/entry TTLs and
  /// local kernel VFS locking.
  pub fn new(target: impl Into<String>, executor: impl Into<String>, source: impl Into<String>, writable: bool, uid: u32, gid: u32) -> Self {
    Self {
      target: target.into(),
      executor: executor.into(),
      source: source.into(),
      writable,
      lock_mode: FsLockMode::Local,
      uid,
      gid,
      attr_ttl: Self::DEFAULT_ATTR_TTL,
      entry_ttl: Self::DEFAULT_ENTRY_TTL,
      case_insensitive: false,
    }
  }
}

/// Detaches the FUSE mount at `target` so the kernel stops forwarding requests
/// and the serving session exits cleanly.
///
/// Uses `umount2(MNT_DETACH)` when privileged (e.g. inside a VM guest `init`),
/// falling back to `fusermount3 -u -z` when unprivileged.
// Reference: https://man7.org/linux/man-pages/man8/fusermount3.8.html
// (read 2026-11-27: `-u` unmounts, `-z` lazily, matching `MNT_DETACH`).
#[cfg(target_os = "linux")]
pub fn detach(target: &str) -> anyhow::Result<()> {
  use anyhow::Context as _;

  let target = target.trim_end_matches('/');
  match nix::mount::umount2(target, nix::mount::MntFlags::MNT_DETACH) {
    Ok(()) => Ok(()),
    Err(nix::errno::Errno::EPERM) => {
      let status = std::process::Command::new("fusermount3").args(["-u", "-z", target]).status().context("cannot run fusermount3")?;
      anyhow::ensure!(status.success(), "fusermount3 -u -z {target}: {status}");
      Ok(())
    }
    Err(e) => Err(anyhow::anyhow!("cannot detach {target}: {e}")),
  }
}

/// Non-Linux stub for [`detach`].
#[cfg(not(target_os = "linux"))]
pub fn detach(target: &str) -> anyhow::Result<()> {
  anyhow::bail!("FUSE mount detach is only supported on Linux (target: {target})")
}

/// Opens an `FsService.Session` stream on `client`, mounts the workspace at
/// `config.target` via FUSE3, and keeps the [`fuse::MountGuard`] alive in a
/// background task until unmounted or the session is cancelled.
#[cfg(target_os = "linux")]
pub async fn mount(client: &needy_api_client::Client, config: MountConfig) -> anyhow::Result<()> {
  let (tx, rx) = client.fs_session().await?;
  let guard = fuse::mount_with_stream(tx, rx, config).await?;
  tokio::spawn(guard.wait());
  Ok(())
}

/// Non-Linux stub for [`mount`].
#[cfg(all(unix, not(target_os = "linux")))]
pub async fn mount(_client: &needy_api_client::Client, config: MountConfig) -> anyhow::Result<()> {
  anyhow::bail!("FUSE3 workspace mounts are only supported on Linux (target: {})", config.target)
}

/// Mounts an executor workspace at `m.target` for `uid:gid`.
///
/// Uses the async FUSE3 `FsService` client by default. Setting
/// `NEEDY_FS_BACKEND=sshfs` opts out to legacy `sshfs`. When `NEEDY_FS_BACKEND`
/// is unset (or not explicitly `"fuse3"`) and the server returns
/// `UNIMPLEMENTED`, this falls back to `sshfs` automatically.
#[cfg(target_os = "linux")]
pub async fn mount_workspace(client: &needy_api_client::Client, m: &crate::sshfs::Mount, uid: u32, gid: u32) -> anyhow::Result<()> {
  let backend = FsBackend::from_env();
  if backend == FsBackend::Sshfs {
    tracing::debug!("mount {}: using sshfs ({FS_BACKEND_ENV}=sshfs)", m.target);
    return crate::sshfs::mount(client, m, uid, gid).await;
  }

  let config = MountConfig::new(m.target.clone(), m.executor.clone(), m.source.clone(), m.writable, uid, gid);
  match mount(client, config).await {
    Ok(()) => Ok(()),
    Err(err) if backend == FsBackend::Auto && is_unimplemented(&err) => {
      tracing::info!("mount {}: FsService unimplemented on server, falling back to sshfs", m.target);
      crate::sshfs::mount(client, m, uid, gid).await
    }
    Err(err) => Err(err),
  }
}
