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

/// The user's home: a tmpfs the user owns.
pub(super) fn home(home: &str, uid: u32, gid: u32) {
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
