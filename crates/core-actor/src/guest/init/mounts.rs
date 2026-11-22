//! Filesystems init mounts: the system's, the user's home, and detaching the
//! workspace mounts before the VM ends.

use nix::mount::{MntFlags, MsFlags};

/// Mounts `fstype` on `target`, creating it. A failure is logged and skipped:
/// the actor boots with what it has.
fn mount(source: &str, target: &str, fstype: &str, flags: MsFlags, data: &str) {
  if let Err(e) = std::fs::create_dir_all(target) {
    tracing::warn!("cannot create {target}: {e}");
  }
  if let Err(e) = nix::mount::mount(Some(source), target, Some(fstype), flags, Some(data)) {
    tracing::warn!("mount {fstype} on {target}: {e}");
  }
}

/// `/proc`, `/sys`, `/dev/pts` and the tmpfs directories. The root
/// filesystem is read-only.
pub(super) fn system() {
  let nosuid = MsFlags::MS_NOSUID | MsFlags::MS_NODEV;
  mount("proc", "/proc", "proc", nosuid | MsFlags::MS_NOEXEC, "");
  mount("sysfs", "/sys", "sysfs", nosuid | MsFlags::MS_NOEXEC, "");
  mount("devpts", "/dev/pts", "devpts", MsFlags::MS_NOSUID, "mode=620,ptmxmode=666");
  mount("tmpfs", "/dev/shm", "tmpfs", nosuid, "mode=1777");
  mount("tmpfs", "/tmp", "tmpfs", nosuid, "mode=1777");
  mount("tmpfs", "/run", "tmpfs", nosuid, "mode=755");
  mount("tmpfs", "/var/tmp", "tmpfs", nosuid, "mode=1777");
}

/// The user's home: a tmpfs the user owns. The image has only its own
/// user's home, and the root is read-only, so another user's home gets a
/// tmpfs at `/home` first to be created in.
// NOTICE: that tmpfs hides the image's own homes, which are empty in needy's
// images: every actor's home is a fresh tmpfs anyway.
pub(super) fn home(home: &str, uid: u32, gid: u32) {
  let path = std::path::Path::new(home);
  if !path.is_dir()
    && let Some(parent) = path.parent()
    && std::fs::create_dir_all(path).is_err()
  {
    let parent = parent.to_string_lossy();
    mount("tmpfs", &parent, "tmpfs", MsFlags::MS_NOSUID | MsFlags::MS_NODEV, "mode=755");
  }
  mount("tmpfs", home, "tmpfs", MsFlags::MS_NOSUID | MsFlags::MS_NODEV, &format!("mode=755,uid={uid},gid={gid}"));
}

/// Detaches the workspace mounts, so the kernel stops asking sshfs.
pub(super) fn detach<'a>(targets: impl Iterator<Item = &'a str>) {
  for target in targets {
    let target = target.trim_end_matches('/');
    if let Err(e) = nix::mount::umount2(target, MntFlags::MNT_DETACH) {
      tracing::warn!("cannot detach {target}: {e}");
    }
  }
}

/// Gives directories on the container's `$HOME` tmpfs to `uid:gid` without
/// crossing into nested bind mounts (`st_dev != home_dev`).
///
/// Container runtimes (`runc`, etc.) create intermediate mount-point parents
/// such as `/home/agent/.local` and `/home/agent/.local/share` as `root:root`
/// (mode 0755) when applying nested host bind mounts (for example sync
/// mappings like `~/.local/share/opencode`), which causes `EACCES` when the
/// driving program creates sibling directories like `~/.local/state`.
pub(super) fn chown_home_ancestors(home: &str, uid: u32, gid: u32) {
  use std::os::unix::fs::MetadataExt as _;

  let root = std::path::Path::new(home);
  let Ok(home_meta) = std::fs::symlink_metadata(root) else {
    return;
  };
  chown_same_dev_dirs(root, home_meta.dev(), uid, gid);
}

fn chown_same_dev_dirs(dir: &std::path::Path, home_dev: u64, uid: u32, gid: u32) {
  use std::os::unix::fs::MetadataExt as _;

  let Ok(entries) = std::fs::read_dir(dir) else {
    return;
  };
  for entry in entries.flatten() {
    let path = entry.path();
    let Ok(meta) = std::fs::symlink_metadata(&path) else {
      continue;
    };
    if !meta.file_type().is_dir() || meta.dev() != home_dev {
      continue;
    }
    if (meta.uid() != uid || meta.gid() != gid)
      && let Err(e) = std::os::unix::fs::lchown(&path, Some(uid), Some(gid))
    {
      tracing::warn!("cannot give {} to {uid}:{gid}: {e}", path.display());
    }
    chown_same_dev_dirs(&path, home_dev, uid, gid);
  }
}

#[cfg(test)]
mod tests {
  use std::os::unix::fs::MetadataExt as _;

  use super::*;

  // ROOT CAUSE:
  //
  // Container runtimes create intermediate directories on the `$HOME` tmpfs
  // (such as `/home/agent/.local` and `/home/agent/.local/share`) as
  // `root:root` (0755) when mounting nested sync mappings like
  // `~/.local/share/opencode`, causing `EACCES` when the driving program
  // creates sibling directories like `~/.local/state`.
  //
  // Before the fix, guest `init` skipped `$HOME` setup for container actors.
  // The fix walks directories on the same filesystem device (`st_dev`) as
  // `$HOME` and chowns them to the profile's user without crossing into bind
  // mounts.
  #[test]
  fn chown_same_dev_dirs_skips_symlinks_and_other_devices() {
    let home = tempfile::tempdir().unwrap();
    let nested = home.path().join(".local/share");
    std::fs::create_dir_all(&nested).unwrap();
    std::os::unix::fs::symlink(&nested, home.path().join("link")).unwrap();

    let meta = std::fs::symlink_metadata(home.path()).unwrap();
    // Passing a mismatched device ID must be a no-op (never descends into
    // mount points on another filesystem).
    chown_same_dev_dirs(home.path(), meta.dev().wrapping_add(1), meta.uid(), meta.gid());
    // Passing the same device ID walks nested directories cleanly without
    // following symlinks or failing on non-root test runs.
    chown_home_ancestors(&home.path().to_string_lossy(), meta.uid(), meta.gid());
    assert!(nested.is_dir());
  }
}
