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
