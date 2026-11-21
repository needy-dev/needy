//! Workspace mounts: sshfs in passive mode, whose SFTP stream travels over a
//! needy exec stream (`Origin::ActorMount`) to `sftp-server` on the executor.
//! The daemon's relay may install the SFTP fault tap on that stream.
//!
//! A VM's init mounts inside the actor; a container runtime mounts on the
//! daemon's machine and bind-mounts the result into the container. Both reach
//! the daemon through the run's guest channel, so the stream is the actor's
//! either way.

// The stream bridge is plain Unix and tested on every Unix; mounting needs
// Linux (FUSE, `/proc/mounts`, `umount2`).
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

mod stream;

#[cfg(target_os = "linux")]
pub use linux::{Mount, detach, mount};

/// Creates `target` and any missing parent directories, setting ownership of
/// only the newly created directories to `uid`:`gid`.
///
/// Pre-existing ancestor directories are left untouched so system directories
/// (such as `/home`, `/mnt`, or `/var`) never have their ownership altered.
#[cfg(unix)]
fn create_mount_target(target: &std::path::Path, uid: u32, gid: u32) -> anyhow::Result<()> {
  use anyhow::Context as _;
  create_mount_target_with(target, |dir| match std::os::unix::fs::chown(dir, Some(uid), Some(gid)) {
    Ok(()) => Ok(()),
    // NOTICE: a non-root daemon mounting on the host for a `linux-container`
    // actor cannot `chown` to another profile UID; inside a VM guest `init`
    // runs as root so `chown` succeeds.
    Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
      tracing::debug!("cannot chown newly created mount dir {} to {uid}:{gid}: {e}", dir.display());
      Ok(())
    }
    Err(e) => Err(e).with_context(|| format!("cannot chown {} to {uid}:{gid}", dir.display())),
  })
}

#[cfg(unix)]
fn create_mount_target_with(
  target: &std::path::Path,
  mut on_created: impl FnMut(&std::path::Path) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
  use anyhow::Context as _;
  let mut missing = Vec::new();
  let mut cursor = target;
  while !cursor.exists() {
    missing.push(cursor.to_path_buf());
    match cursor.parent() {
      Some(parent) if !parent.as_os_str().is_empty() && parent != cursor => cursor = parent,
      _ => break,
    }
  }
  for dir in missing.into_iter().rev() {
    match std::fs::create_dir(&dir) {
      Ok(()) => on_created(&dir)?,
      Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
      Err(e) => return Err(e).with_context(|| format!("cannot create {}", dir.display())),
    }
  }
  Ok(())
}

#[cfg(target_os = "linux")]
mod linux {
  use std::path::Path;
  use std::process::Stdio;
  use std::sync::Arc;
  use std::time::Duration;

  use anyhow::{Context, Result};
  use backon::{ConstantBuilder, Retryable};
  use needy_api_client::exec::ExecStream;
  use needy_api_client::{Client, Executor};
  use needy_api_proto::v1::{ExecStart, Origin};

  use super::{create_mount_target, stream};

  /// An executor directory to mount.
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct Mount {
    /// Where it is mounted on this machine.
    pub target: String,
    pub executor: String,
    /// The directory on the executor.
    pub source: String,
    pub writable: bool,
    /// The executor's `sftp-server` command.
    pub sftp_command: Vec<String>,
  }

  /// Mounts an executor directory at `m.target`, owned by the user `uid`:`gid`.
  /// The sshfs process and its bridges run until the mount is detached.
  // NOTICE: the bridge tasks are spawned on the current runtime and end with
  // the mount (sshfs exits once it is detached, which ends the stream, or the
  // stream stays lost past its window); they have no owner of their own.
  pub async fn mount(client: &Client, m: &Mount, uid: u32, gid: u32) -> Result<()> {
    create_mount_target(Path::new(&m.target), uid, gid)?;
    let mut options = format!("passive,allow_other,uid={uid},gid={gid},idmap=none,cache=yes,kernel_cache,attr_timeout=1,entry_timeout=1");
    if !m.writable {
      options.push_str(",ro");
    }
    let mut sshfs = tokio::process::Command::new("sshfs")
      .args(["-f", "-o", &options, &format!(":{}", m.source), &m.target])
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::inherit())
      .spawn()
      .context("cannot start sshfs")?;
    let sshfs_in = sshfs.stdin.take().context("sshfs stdin")?;
    let sshfs_out = sshfs.stdout.take().context("sshfs stdout")?;
    let start = ExecStart {
      argv: m.sftp_command.clone(),
      executor: m.executor.clone(),
      // The mount root lets file faults match paths relative to it.
      cwd: m.source.clone(),
      origin: Origin::ActorMount.into(),
      ..Default::default()
    };
    let first = ExecStream::start(client, start.clone()).await?;
    let executor: Arc<dyn Executor> = Arc::new(client.clone());
    // sshfs's requests go to the executor's stdin, its replies back to sshfs,
    // across lost streams (see `stream`).
    tokio::spawn(stream::bridge(executor, start, first, sshfs_out, sshfs_in, m.target.clone(), stream::RECONNECT_WINDOW));
    let target = m.target.clone();
    tokio::spawn(async move {
      match sshfs.wait().await {
        Ok(status) if status.success() => tracing::debug!("mount {target}: sshfs exited"),
        Ok(status) => tracing::warn!("mount {target}: sshfs exited with {status}"),
        Err(e) => tracing::warn!("mount {target}: cannot wait for sshfs: {e}"),
      }
    });
    let target = m.target.trim_end_matches('/');
    // NOTICE: sshfs mounts after its first SFTP round trip; 100 retries
    // 100 ms apart allow 10 s for a slow executor.
    let retry = ConstantBuilder::default().with_delay(Duration::from_millis(100)).with_max_times(100);
    (|| async { if mounted(target) { Ok(()) } else { Err(()) } })
      .retry(retry)
      .await
      .map_err(|()| anyhow::anyhow!("sshfs did not mount within 10 s"))
  }

  /// Whether `/proc/mounts` lists `target`.
  fn mounted(target: &str) -> bool {
    match std::fs::read_to_string("/proc/mounts") {
      Ok(mounts) => mounts.lines().any(|line| line.split(' ').nth(1) == Some(target)),
      Err(e) => {
        tracing::debug!("cannot read /proc/mounts: {e}");
        false
      }
    }
  }

  /// Detaches the mount at `target`, so the kernel stops asking sshfs and
  /// sshfs exits. Unprivileged, it asks `fusermount3`, which mounted it.
  // Reference: https://man7.org/linux/man-pages/man8/fusermount3.8.html
  // (read 2026-11-11: `-u` unmounts, `-z` lazily, as `MNT_DETACH` does).
  pub fn detach(target: &str) -> Result<()> {
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
}

#[cfg(all(test, unix))]
mod tests {
  use std::os::unix::fs::MetadataExt as _;
  use std::path::PathBuf;

  use super::{create_mount_target, create_mount_target_with};

  // ROOT CAUSE:
  //
  // Guest `init` runs as root (PID 1) and previously called
  // `std::fs::create_dir_all(&m.target)` before mounting `sshfs`. If
  // `m.target` had missing parent directories (e.g. `~/.pi` for `~/.pi/agent`),
  // they were created owned by `root:root`, causing `Permission denied` when
  // the run's non-root user wrote sibling files inside `~/.pi`.
  //
  // Before the fix, newly created ancestor directories stayed owned by root.
  // The fix creates missing directories one component at a time and chowns
  // only the newly created directories to the target `uid:gid`, leaving
  // pre-existing ancestors untouched.
  #[test]
  fn create_mount_target_chowns_only_newly_created_directories() {
    let root = tempfile::tempdir().unwrap();
    let existing_home = root.path().join("home/agent");
    std::fs::create_dir_all(&existing_home).unwrap();
    let target = existing_home.join(".pi/agent");

    let mut chowned: Vec<PathBuf> = Vec::new();
    create_mount_target_with(&target, |dir| {
      chowned.push(dir.to_path_buf());
      Ok(())
    })
    .unwrap();

    assert!(target.is_dir());
    assert_eq!(chowned, vec![existing_home.join(".pi"), existing_home.join(".pi/agent")]);

    // A second call when everything already exists touches nothing.
    let mut second = Vec::new();
    create_mount_target_with(&target, |dir| {
      second.push(dir.to_path_buf());
      Ok(())
    })
    .unwrap();
    assert!(second.is_empty());
  }

  #[test]
  fn create_mount_target_sets_uid_gid_on_newly_created_ancestors() {
    let root = tempfile::tempdir().unwrap();
    let existing = root.path().join("existing");
    std::fs::create_dir_all(&existing).unwrap();
    let existing_meta = std::fs::metadata(&existing).unwrap();
    let target = existing.join(".pi/agent");

    // When running as root (e.g. inside the Linux test container), verify real
    // chown to a distinct non-root uid:gid while leaving `existing` root-owned.
    let (want_uid, want_gid) = if existing_meta.uid() == 0 {
      (1234, 5678)
    } else {
      (existing_meta.uid(), existing_meta.gid())
    };

    create_mount_target(&target, want_uid, want_gid).unwrap();

    let parent_meta = std::fs::metadata(existing.join(".pi")).unwrap();
    let target_meta = std::fs::metadata(&target).unwrap();
    let existing_after = std::fs::metadata(&existing).unwrap();
    assert_eq!((parent_meta.uid(), parent_meta.gid()), (want_uid, want_gid));
    assert_eq!((target_meta.uid(), target_meta.gid()), (want_uid, want_gid));
    assert_eq!((existing_after.uid(), existing_after.gid()), (existing_meta.uid(), existing_meta.gid()));
  }
}
