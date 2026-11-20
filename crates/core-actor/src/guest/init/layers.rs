//! Stacking layer disks over the base root in a VM actor, and archiving
//! and streaming a kept session's upper layer when the session ends.
//!
//! After `GetConfig`, when the configuration names layers or keeps the
//! session's upper layer, init finds the attached SquashFS layer disks by
//! their [`crate::contract::LAYER_MARKER`] file, mounts them read-only
//! under `/run/needy/layers`, stacks them top-to-bottom over the base root
//! with `overlayfs` and a `tmpfs` upper layer, moves its earlier mounts
//! (including `/.needy/disk` and `/.needy/needy` so `binfmt_needy` keeps
//! never-routing needy's own programs) into the new root, and switches root
//! with `pivot_root(".", ".")`.

use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};

use anyhow::{Context, Result, bail};
use needy_api_client::Client;
use needy_api_proto::v1::GuestLayer;
use nix::mount::{MntFlags, MsFlags};
use tokio::io::{AsyncRead, ReadBuf};
use tokio::sync::mpsc;

use crate::contract::{GUEST, LAYER_MARKER, LAYER_ROOT, NEEDY_DIR};

/// Where init mounts the base root bind and the discovered layer disks.
const LAYERS_DIR: &str = "/run/needy/layers";
/// Dedicated tmpfs holding overlayfs's `upper` and `work` directories, so
/// `statvfs` measures the session's upper layer directly.
const OVERLAY_DIR: &str = "/run/needy/overlay";
/// Temporary mountpoint for the merged overlayfs root before `pivot_root`.
/// Kept under `/tmp` (not `/run`) so `/run` can be `MS_MOVE`d into it.
const NEW_ROOT: &str = "/tmp/needy-root";
/// Where `boot.c` (`images/stub/boot.c`) mounts the guest disk `/dev/vdb`.
const GUEST_DISK_DIR: &str = "/.needy/disk";

/// The stacked overlay root's upper-layer paths after `pivot_root`.
#[derive(Debug, Clone)]
pub(super) struct Stacked {
  /// The tmpfs mountpoint holding `upper` and `work`.
  pub overlay: PathBuf,
  /// The overlayfs `upperdir`.
  pub upper: PathBuf,
}

impl Stacked {
  /// Logs the upper-layer tmpfs usage and warns when it nears capacity.
  pub(super) fn log_usage(&self) {
    // TODO(images): a session's upper layer lives in guest memory (tmpfs);
    // replace with a host-backed scratch disk when edit or build sessions
    // outgrow guest RAM (see
    // `docs/ai/references/architecture/2026-11-17-images-and-run-data-spec.md`).
    match nix::sys::statvfs::statvfs(&self.overlay) {
      Ok(stat) => {
        let block = stat.fragment_size();
        let total = stat.blocks().saturating_mul(block);
        let avail = stat.blocks_available().saturating_mul(block);
        let used = total.saturating_sub(avail);
        if total > 0 && used.saturating_mul(5) >= total.saturating_mul(4) {
          tracing::warn!("upper layer tmpfs is nearly full: {used} of {total} bytes used");
        } else {
          tracing::info!("upper layer tmpfs: {used} of {total} bytes used");
        }
      }
      Err(e) => tracing::warn!("cannot check upper layer tmpfs usage at {}: {e}", self.overlay.display()),
    }
  }

  /// Archives the session's upper layer and streams it to the daemon via
  /// `GuestService.PushLayer`, never holding the archive whole in memory.
  pub(super) async fn push(&self, client: &Client) -> Result<()> {
    self.log_usage();
    let (reader, task) = archive_stream(self.upper.clone());
    let pushed = needy_api_client::guest::push_layer(client, reader).await;
    let archived = task.await.context("upper-layer archiver task failed")?;
    archived?;
    pushed.context("cannot upload the session's upper layer")?;
    Ok(())
  }
}

/// Stacks `layers` (lowest first) over the base root with `overlayfs`,
/// moves init's earlier mounts and `/.needy` into it, and switches `/` to
/// the new root.
pub(super) fn stack(layers: &[GuestLayer]) -> Result<Stacked> {
  // Ensure mount propagation is private before moving mounts and pivoting.
  if let Err(e) = nix::mount::mount(None::<&str>, "/", None::<&str>, MsFlags::MS_REC | MsFlags::MS_PRIVATE, None::<&str>) {
    tracing::debug!("cannot set / mount propagation to private: {e}");
  }

  // Non-recursive bind of `/` gives overlayfs the bare base rootfs without
  // `/proc`, `/sys`, `/dev`, `/run`, or `/.needy/disk` underneath it.
  let base_mount = Path::new(LAYERS_DIR).join("base");
  std::fs::create_dir_all(&base_mount).with_context(|| format!("cannot create {}", base_mount.display()))?;
  nix::mount::mount(Some("/"), &base_mount, None::<&str>, MsFlags::MS_BIND, None::<&str>)
    .with_context(|| format!("cannot bind-mount / to {}", base_mount.display()))?;

  let mut lowers = if layers.is_empty() {
    Vec::new()
  } else {
    let mounted = discover_layer_disks(Path::new("/dev"), &Path::new(LAYERS_DIR).join("dev"), layers)?;
    resolve_layer_roots(layers, &mounted)?
  };
  lowers.push(base_mount);

  let overlay = PathBuf::from(OVERLAY_DIR);
  std::fs::create_dir_all(&overlay).with_context(|| format!("cannot create {}", overlay.display()))?;
  // TODO(images): a session's upper layer is a tmpfs in guest memory;
  // switch to a host-backed scratch disk when sessions exceed guest RAM
  // (see `docs/ai/references/architecture/2026-11-17-images-slice-1-plan.md`).
  nix::mount::mount(Some("tmpfs"), &overlay, Some("tmpfs"), MsFlags::empty(), Some("mode=755"))
    .with_context(|| format!("cannot mount upper-layer tmpfs on {}", overlay.display()))?;
  let upper = overlay.join("upper");
  let work = overlay.join("work");
  std::fs::create_dir_all(&upper).with_context(|| format!("cannot create {}", upper.display()))?;
  std::fs::create_dir_all(&work).with_context(|| format!("cannot create {}", work.display()))?;

  let new_root = Path::new(NEW_ROOT);
  std::fs::create_dir_all(new_root).with_context(|| format!("cannot create {}", new_root.display()))?;
  mount_overlay(&lowers, &upper, &work, new_root)?;

  // Move earlier mounts into the new root. `/dev` brings `/dev/pts` and
  // `/dev/shm`; `/run` brings `/run/needy/layers` and `/run/needy/overlay`.
  // Moving `/.needy/disk` and `/.needy/needy` preserves init's
  // `d_path(&exe->f_path) == "/.needy/needy"` so `binfmt_needy`'s
  // `caller_is_needy()` keeps skipping needy's own execs.
  for path in [
    "/proc",
    "/sys",
    "/dev",
    "/var/tmp",
    GUEST_DISK_DIR,
    GUEST,
    "/run",
  ] {
    move_mount(Path::new(path), &new_root.join(path.trim_start_matches('/')))?;
  }

  // `pivot_root(".", ".")` avoids creating a `put_old` directory inside the
  // overlayfs upper layer.
  // Reference: https://man7.org/linux/man-pages/man2/pivot_root.2.html
  // ("pivot_root('.', '.')")
  std::env::set_current_dir(new_root).with_context(|| format!("cannot chdir to {}", new_root.display()))?;
  nix::unistd::pivot_root(".", ".").context("pivot_root into stacked overlay root failed")?;
  nix::mount::umount2(".", MntFlags::MNT_DETACH).context("cannot detach old root after pivot_root")?;
  std::env::set_current_dir("/").context("cannot chdir to / after pivot_root")?;

  // `/tmp` was left behind on the old root; mount a fresh tmpfs on the new
  // root's `/tmp`.
  let nosuid = MsFlags::MS_NOSUID | MsFlags::MS_NODEV;
  if !Path::new("/tmp").exists() {
    std::fs::create_dir_all("/tmp").context("cannot create /tmp")?;
  }
  nix::mount::mount(Some("tmpfs"), "/tmp", Some("tmpfs"), nosuid, Some("mode=1777")).context("cannot mount /tmp after pivot_root")?;

  // Sanity check that `/.needy/needy` survived the pivot.
  if !Path::new(NEEDY_DIR).join("needy").exists() {
    bail!("{GUEST} is missing after switching to the stacked root");
  }

  let stacked = Stacked { overlay, upper };
  stacked.log_usage();
  Ok(stacked)
}

/// Moves a mount from `source` to `target` with `MS_MOVE`. Only creates
/// `target` if it does not already exist in the lower layers, so mounting
/// over an existing lower directory or file does not trigger a copy-up into
/// the overlayfs upper layer.
fn move_mount(source: &Path, target: &Path) -> Result<()> {
  if !source.exists() {
    tracing::debug!("skipping move of absent mount {}", source.display());
    return Ok(());
  }
  if target.symlink_metadata().is_err() {
    if source.is_dir() {
      std::fs::create_dir_all(target).with_context(|| format!("cannot create {}", target.display()))?;
    } else {
      if let Some(parent) = target.parent()
        && !parent.exists()
      {
        std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
      }
      File::create(target).with_context(|| format!("cannot create {}", target.display()))?;
    }
  }
  nix::mount::mount(Some(source), target, None::<&str>, MsFlags::MS_MOVE, None::<&str>)
    .with_context(|| format!("cannot move mount {} to {}", source.display(), target.display()))
}

/// Whether `name` in `/dev` is a candidate virtio-blk layer disk: `vd`
/// followed by lowercase letters, excluding `vda` (the root disk) and `vdb`
/// (the guest disk `images/stub/boot.c` mounts).
fn is_layer_device_name(name: &str) -> bool {
  let Some(suffix) = name.strip_prefix("vd") else {
    return false;
  };
  !suffix.is_empty() && suffix != "a" && suffix != "b" && suffix.bytes().all(|b| b.is_ascii_lowercase())
}

/// Scans `dev_dir` for attached layer disks, mounts each read-only SquashFS
/// under `mount_root`, reads [`LAYER_MARKER`], and keeps those matching
/// `expected`. Unmounts non-matching or duplicate disks.
fn discover_layer_disks(dev_dir: &Path, mount_root: &Path, expected: &[GuestLayer]) -> Result<HashMap<String, PathBuf>> {
  let wanted: HashSet<&str> = expected.iter().map(|l| l.digest.as_str()).collect();
  let mut candidates = Vec::new();
  for entry in std::fs::read_dir(dev_dir).with_context(|| format!("cannot list {}", dev_dir.display()))? {
    let entry = entry?;
    let name = entry.file_name();
    let Some(name_str) = name.to_str() else {
      continue;
    };
    if !is_layer_device_name(name_str) {
      continue;
    }
    let path = entry.path();
    if path.metadata().is_ok_and(|m| m.file_type().is_block_device()) {
      candidates.push((name_str.to_string(), path));
    }
  }
  candidates.sort_by(|a, b| a.0.cmp(&b.0));

  let mut mounted: HashMap<String, PathBuf> = HashMap::new();
  let flags = MsFlags::MS_RDONLY | MsFlags::MS_NOSUID | MsFlags::MS_NODEV;
  for (name, dev_path) in candidates {
    let target = mount_root.join(&name);
    std::fs::create_dir_all(&target).with_context(|| format!("cannot create {}", target.display()))?;
    if let Err(e) = nix::mount::mount(Some(&dev_path), &target, Some("squashfs"), flags, None::<&str>) {
      tracing::debug!("skipping {}: squashfs mount failed: {e}", dev_path.display());
      let _ = std::fs::remove_dir(&target);
      continue;
    }
    let marker_path = target.join(LAYER_MARKER);
    let digest = match std::fs::read_to_string(&marker_path) {
      Ok(text) => text.trim().to_string(),
      Err(e) => {
        tracing::debug!("skipping {}: cannot read {}: {e}", dev_path.display(), marker_path.display());
        let _ = nix::mount::umount2(&target, MntFlags::MNT_DETACH);
        let _ = std::fs::remove_dir(&target);
        continue;
      }
    };
    if !wanted.contains(digest.as_str()) || mounted.contains_key(&digest) {
      tracing::debug!("unmounting unused layer disk {} ({digest})", dev_path.display());
      let _ = nix::mount::umount2(&target, MntFlags::MNT_DETACH);
      let _ = std::fs::remove_dir(&target);
      continue;
    }
    tracing::info!("mounted layer disk {} for {digest}", dev_path.display());
    mounted.insert(digest, target);
  }
  Ok(mounted)
}

/// Resolves `expected` (lowest first) against `mounted` (`digest -> disk
/// mountpoint`), returning each layer's [`LAYER_ROOT`] directory in
/// overlayfs `lowerdir` order (top-to-bottom). Fails clearly if any
/// expected digest is missing or lacks [`LAYER_ROOT`].
fn resolve_layer_roots(expected: &[GuestLayer], mounted: &HashMap<String, PathBuf>) -> Result<Vec<PathBuf>> {
  let missing: Vec<&str> = expected.iter().map(|l| l.digest.as_str()).filter(|d| !mounted.contains_key(*d)).collect();
  if !missing.is_empty() {
    let mut found: Vec<&str> = mounted.keys().map(String::as_str).collect();
    found.sort_unstable();
    bail!(
      "missing expected layer disk(s): {} (attached layer disks found: {})",
      missing.join(", "),
      if found.is_empty() {
        "none".to_string()
      } else {
        found.join(", ")
      }
    );
  }
  let mut roots = Vec::with_capacity(expected.len());
  // `expected` is lowest first; overlayfs `lowerdir` is top-to-bottom.
  for layer in expected.iter().rev() {
    let mount = &mounted[&layer.digest];
    let root = mount.join(LAYER_ROOT);
    if !root.is_dir() {
      bail!("layer disk for {} at {} is missing its {LAYER_ROOT}/ directory", layer.digest, mount.display());
    }
    roots.push(root);
  }
  Ok(roots)
}

/// Mounts `overlayfs` onto `merged` with `lowers` (top-to-bottom), `upper`,
/// and `work`. Disables `redirect_dir` and `metacopy` so the upper
/// directory is always a self-contained layer for `Store::put`.
// Reference: https://github.com/torvalds/linux/blob/v6.18/Documentation/filesystems/overlayfs.rst
// ("Multiple lower layers": rightmost is lowest, leftmost is highest).
fn mount_overlay(lowers: &[PathBuf], upper: &Path, work: &Path, merged: &Path) -> Result<()> {
  if lowers.is_empty() {
    bail!("overlayfs requires at least one lower directory");
  }
  let lowerdir = lowers.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>().join(":");
  let options = format!("lowerdir={lowerdir},upperdir={},workdir={},redirect_dir=off,metacopy=off", upper.display(), work.display());
  nix::mount::mount(Some("overlay"), merged, Some("overlay"), MsFlags::empty(), Some(options.as_str()))
    .with_context(|| format!("cannot mount overlayfs on {} ({options})", merged.display()))
}

/// Archives an overlayfs `upper` directory into `out` as an uncompressed
/// tar archive in the format `crate::image::store::Store::put` accepts:
///
/// - Overlayfs whiteouts (char device 0/0) are written as `EntryType::Char`
///   with major 0, minor 0.
/// - Overlayfs opaque directories (xattr `trusted.overlay.opaque=y` or
///   `user.overlay.opaque=y`) carry the corresponding `SCHILY.xattr.*` PAX
///   extended header.
/// - Hard links within `upper` are preserved via `(dev, ino)` tracking.
/// - Unix sockets are skipped.
pub(super) fn archive_upper(upper: &Path, out: impl Write) -> Result<()> {
  let mut builder = tar::Builder::new(out);
  let mut seen_inodes: HashMap<(u64, u64), PathBuf> = HashMap::new();
  walk_upper(&mut builder, upper, Path::new(""), &mut seen_inodes)?;
  builder.finish().context("cannot finish upper-layer tar archive")?;
  Ok(())
}

fn walk_upper<W: Write>(
  builder: &mut tar::Builder<W>,
  root: &Path,
  rel: &Path,
  seen_inodes: &mut HashMap<(u64, u64), PathBuf>,
) -> Result<()> {
  let dir = if rel.as_os_str().is_empty() {
    root.to_path_buf()
  } else {
    root.join(rel)
  };
  let mut entries: Vec<_> = std::fs::read_dir(&dir)
    .with_context(|| format!("cannot list {}", dir.display()))?
    .collect::<io::Result<Vec<_>>>()
    .with_context(|| format!("cannot read entry in {}", dir.display()))?;
  entries.sort_by_key(|e| e.file_name());

  for entry in entries {
    let name = entry.file_name();
    let child_rel = rel.join(&name);
    let child_abs = entry.path();
    let meta = std::fs::symlink_metadata(&child_abs).with_context(|| format!("cannot stat {}", child_abs.display()))?;
    let ft = meta.file_type();

    if ft.is_socket() {
      tracing::debug!("skipping socket {} in upper layer", child_rel.display());
      continue;
    }

    if ft.is_dir() {
      if let Some(attr) = opaque_xattr(&child_abs)? {
        builder
          .append_pax_extensions([(attr, b"y".as_slice())])
          .with_context(|| format!("cannot write opaque PAX header for {}", child_rel.display()))?;
      }
      let mut header = header_from_meta(&meta, tar::EntryType::Directory);
      builder
        .append_data(&mut header, &child_rel, io::empty())
        .with_context(|| format!("cannot archive directory {}", child_rel.display()))?;
      walk_upper(builder, root, &child_rel, seen_inodes)?;
      continue;
    }

    // Preserve hard links among non-directory entries within the upper layer.
    if meta.nlink() > 1 {
      let key = (meta.dev(), meta.ino());
      if let Some(first) = seen_inodes.get(&key) {
        let mut header = header_from_meta(&meta, tar::EntryType::Link);
        builder
          .append_link(&mut header, &child_rel, first)
          .with_context(|| format!("cannot archive hard link {} -> {}", child_rel.display(), first.display()))?;
        continue;
      }
      seen_inodes.insert(key, child_rel.clone());
    }

    if ft.is_file() {
      let mut header = header_from_meta(&meta, tar::EntryType::Regular);
      header.set_size(meta.len());
      let mut file = File::open(&child_abs).with_context(|| format!("cannot open {}", child_abs.display()))?;
      builder.append_data(&mut header, &child_rel, &mut file).with_context(|| format!("cannot archive file {}", child_rel.display()))?;
    } else if ft.is_symlink() {
      let target = std::fs::read_link(&child_abs).with_context(|| format!("cannot read symlink {}", child_abs.display()))?;
      let mut header = header_from_meta(&meta, tar::EntryType::Symlink);
      builder.append_link(&mut header, &child_rel, &target).with_context(|| format!("cannot archive symlink {}", child_rel.display()))?;
    } else if ft.is_char_device() {
      let (major, minor) = (libc::major(meta.rdev()) as u32, libc::minor(meta.rdev()) as u32);
      let mut header = header_from_meta(&meta, tar::EntryType::Char);
      header.set_device_major(major).context("cannot set char major")?;
      header.set_device_minor(minor).context("cannot set char minor")?;
      builder
        .append_data(&mut header, &child_rel, io::empty())
        .with_context(|| format!("cannot archive char device {}", child_rel.display()))?;
    } else if ft.is_block_device() {
      let (major, minor) = (libc::major(meta.rdev()) as u32, libc::minor(meta.rdev()) as u32);
      let mut header = header_from_meta(&meta, tar::EntryType::Block);
      header.set_device_major(major).context("cannot set block major")?;
      header.set_device_minor(minor).context("cannot set block minor")?;
      builder
        .append_data(&mut header, &child_rel, io::empty())
        .with_context(|| format!("cannot archive block device {}", child_rel.display()))?;
    } else if ft.is_fifo() {
      let mut header = header_from_meta(&meta, tar::EntryType::Fifo);
      builder.append_data(&mut header, &child_rel, io::empty()).with_context(|| format!("cannot archive fifo {}", child_rel.display()))?;
    }
  }
  Ok(())
}

fn header_from_meta(meta: &std::fs::Metadata, kind: tar::EntryType) -> tar::Header {
  let mut header = tar::Header::new_gnu();
  header.set_entry_type(kind);
  header.set_mode(meta.mode() & 0o7777);
  header.set_uid(u64::from(meta.uid()));
  header.set_gid(u64::from(meta.gid()));
  header.set_mtime(u64::try_from(meta.mtime()).unwrap_or(0));
  header.set_size(0);
  header
}

/// Returns the PAX extended-attribute key if `dir` is an overlayfs opaque
/// directory (`trusted.overlay.opaque=y` or `user.overlay.opaque=y`).
// Reference: https://github.com/torvalds/linux/blob/v6.18/Documentation/filesystems/overlayfs.rst
// ("whiteouts and opaque directories") and
// https://www.gnu.org/software/tar/manual/html_node/Extensions.html
// (`SCHILY.xattr.NAME`).
fn opaque_xattr(dir: &Path) -> Result<Option<&'static str>> {
  let c_path = CString::new(dir.as_os_str().as_bytes()).with_context(|| format!("path contains NUL: {}", dir.display()))?;
  for (attr, pax_key) in [
    (c"trusted.overlay.opaque", "SCHILY.xattr.trusted.overlay.opaque"),
    (c"user.overlay.opaque", "SCHILY.xattr.user.overlay.opaque"),
  ] {
    let mut buf = [0u8; 4];
    // SAFETY: `c_path` and `attr` are NUL-terminated C strings; `buf` is a
    // valid writable buffer of `buf.len()` bytes.
    let len = unsafe { libc::lgetxattr(c_path.as_ptr(), attr.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    if len >= 0 {
      if &buf[..len as usize] == b"y" {
        return Ok(Some(pax_key));
      }
      continue;
    }
    let err = io::Error::last_os_error();
    if matches!(err.raw_os_error(), Some(libc::ENODATA | libc::EOPNOTSUPP)) {
      continue;
    }
    return Err(err).with_context(|| format!("cannot read xattr on {}", dir.display()));
  }
  Ok(None)
}

/// Starts archiving `upper` on a blocking thread and returns an
/// [`AsyncRead`] streaming its bytes. An error during archiving is sent
/// through the channel as an `io::Error` rather than closing at EOF, so
/// `needy_api_client::guest::push_layer` never sends `last: true` on a
/// broken archive.
fn archive_stream(upper: PathBuf) -> (ChannelReader, tokio::task::JoinHandle<Result<()>>) {
  let (tx, rx) = mpsc::channel(4);
  let task = tokio::task::spawn_blocking(move || {
    let mut writer = BufWriter::with_capacity(256 * 1024, ChannelWriter { tx: tx.clone() });
    let result = archive_upper(&upper, &mut writer).and_then(|()| writer.flush().context("cannot flush upper-layer archive"));
    if let Err(ref e) = result {
      let _ = tx.blocking_send(Err(io::Error::other(format!("{e:#}"))));
    }
    result
  });
  (
    ChannelReader {
      rx,
      buf: Vec::new(),
      pos: 0,
    },
    task,
  )
}

struct ChannelWriter {
  tx: mpsc::Sender<io::Result<Vec<u8>>>,
}

impl Write for ChannelWriter {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    if buf.is_empty() {
      return Ok(0);
    }
    self.tx.blocking_send(Ok(buf.to_vec())).map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "layer upload receiver closed"))?;
    Ok(buf.len())
  }

  fn flush(&mut self) -> io::Result<()> {
    Ok(())
  }
}

struct ChannelReader {
  rx: mpsc::Receiver<io::Result<Vec<u8>>>,
  buf: Vec<u8>,
  pos: usize,
}

impl AsyncRead for ChannelReader {
  fn poll_read(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>, out: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
    loop {
      if self.pos < self.buf.len() {
        let take = (self.buf.len() - self.pos).min(out.remaining());
        out.put_slice(&self.buf[self.pos..self.pos + take]);
        self.pos += take;
        if self.pos == self.buf.len() {
          self.buf.clear();
          self.pos = 0;
        }
        return Poll::Ready(Ok(()));
      }
      match self.rx.poll_recv(cx) {
        Poll::Pending => return Poll::Pending,
        Poll::Ready(None) => return Poll::Ready(Ok(())),
        Poll::Ready(Some(Err(e))) => return Poll::Ready(Err(e)),
        Poll::Ready(Some(Ok(chunk))) => {
          self.buf = chunk;
          self.pos = 0;
        }
      }
    }
  }
}

#[cfg(test)]
#[path = "layers_test.rs"]
mod tests;
