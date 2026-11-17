//! A layer disk: a layer as a SquashFS image a VM driver attaches read-only
//! and the guest stacks with overlayfs.
//!
//! Its root holds the marker [`crate::contract::LAYER_MARKER`], which names
//! the layer's digest, and the layer's tree under
//! [`crate::contract::LAYER_ROOT`], with its deletions in overlayfs's form:
//! a whiteout is a character device 0/0. The SquashFS writer writes no
//! extended attributes, so an opaque directory cannot be marked as one; its
//! disk whites out instead every entry the layers below still show in it.
// NOTICE: `backhand` writes SquashFS in Rust on Linux and macOS alike, where
// `macos-vz` builds its disks and no `mksquashfs` is at hand; both actor
// kernels read zlib SquashFS (`guest::disk` has the configuration lines).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::archive::{Entry, Index, Kind};
use super::{Digest, Error, Result, io};
use crate::contract::{LAYER_MARKER, LAYER_ROOT};

/// What the layers below a layer show: each visible path, and whether it is
/// a directory.
#[derive(Debug, Default)]
pub(super) struct Below {
  view: BTreeMap<PathBuf, bool>,
}

impl Below {
  /// Puts `layer` on top of the layers so far, as overlayfs (and an OCI
  /// runtime) would: its whiteouts and opaque directories hide what is
  /// below, then its entries show.
  pub fn stack(&mut self, layer: &Index) {
    for removed in &layer.whiteouts {
      self.remove(removed, true);
    }
    for dir in &layer.opaque {
      self.remove(dir, false);
    }
    for (at, entry) in &layer.entries {
      if !entry.is_dir() {
        self.remove(at, false);
      }
      for parent in at.ancestors().skip(1).filter(|parent| !parent.as_os_str().is_empty()) {
        self.view.entry(parent.to_path_buf()).or_insert(true);
      }
      self.view.insert(at.clone(), entry.is_dir());
    }
  }

  /// Hides what is under `at`, and `at` itself with `itself`.
  fn remove(&mut self, at: &Path, itself: bool) {
    let hidden: Vec<_> = self.descendants(at).filter(|(path, _)| itself || path.as_path() != at).map(|(path, _)| path.clone()).collect();
    for path in hidden {
      self.view.remove(&path);
    }
  }

  /// `at` and every visible path under it, in order.
  // NOTICE: a `PathBuf` orders by components, so a path's descendants come
  // right after it, before any sibling (`a`, `a/b`, `a-b`).
  fn descendants<'a>(&'a self, at: &'a Path) -> impl Iterator<Item = (&'a PathBuf, &'a bool)> + 'a {
    self.view.range(at.to_path_buf()..).take_while(move |(path, _)| path.starts_with(at))
  }
}

/// Whether `layer`'s disk depends on the layers below it: it has an opaque
/// directory, or puts a directory back where it removed one.
pub(super) fn hides_below(layer: &Index) -> bool {
  !layer.opaque.is_empty() || layer.whiteouts.iter().any(|removed| layer.entries.get(removed).is_some_and(Entry::is_dir))
}

enum Node {
  Entry(Entry),
  Whiteout,
}

/// Writes the disk of `layer`, whose digest is `digest`, over the layers
/// `below`, to `path`: beside it, then renamed.
pub(super) fn write(layer: &Index, digest: &Digest, below: &Below, path: &Path) -> Result<()> {
  let mut tree = BTreeMap::new();
  for at in layer.entries.keys() {
    // SquashFS has hard links, but `backhand` writes none: a hard link is
    // written as a copy, whose blocks the writer stores once.
    tree.insert(at.clone(), Node::Entry(layer.resolve(at)?));
  }
  let mut opaque: BTreeSet<PathBuf> = layer.opaque.clone();
  for removed in &layer.whiteouts {
    match layer.entries.get(removed) {
      // Removed from below and put back as a directory in this layer: what
      // it had below stays hidden, as under an opaque directory.
      Some(entry) if entry.is_dir() => {
        opaque.insert(removed.clone());
      }
      Some(_) => {}
      None => {
        tree.insert(removed.clone(), Node::Whiteout);
      }
    }
  }
  for dir in &opaque {
    expand(&mut tree, below, dir);
  }

  let failed = |message: String| Error::Invalid(format!("cannot write the layer disk {}: {message}", path.display()));
  let mut fs = backhand::FilesystemWriter::default();
  let gzip = backhand::FilesystemCompressor::new(backhand::compression::Compressor::Gzip, None).map_err(|e| failed(e.to_string()))?;
  fs.set_compressor(gzip);
  fs.set_root_mode(0o755);
  let root_owned = |mode| backhand::NodeHeader::new(mode, 0, 0, 0);
  let marker = format!("{digest}\n").into_bytes();
  fs.push_file(std::io::Cursor::new(marker), LAYER_MARKER, root_owned(0o644)).map_err(|e| failed(e.to_string()))?;
  fs.push_dir(LAYER_ROOT, root_owned(0o755)).map_err(|e| failed(e.to_string()))?;
  for (at, node) in tree {
    let target = Path::new(LAYER_ROOT).join(&at);
    let pushed = match node {
      Node::Whiteout => fs.push_char_device(0, &target, root_owned(0)),
      Node::Entry(entry) => {
        // A layer may leave out a directory it only adds to.
        if let Some(parent) = target.parent() {
          fs.push_dir_all(parent, root_owned(0o755)).map_err(|e| failed(format!("{}: {e}", at.display())))?;
        }
        let header = header(&entry).map_err(|why| failed(format!("{}: {why}", at.display())))?;
        match entry.kind {
          Kind::File(data) => fs.push_file(data.reader(), &target, header),
          Kind::Dir => fs.push_dir(&target, header),
          Kind::Symlink(link) => fs.push_symlink(link, &target, header),
          Kind::Char(major, minor) => fs.push_char_device(device_number(major, minor), &target, header),
          Kind::Block(major, minor) => fs.push_block_device(device_number(major, minor), &target, header),
          Kind::Fifo => fs.push_fifo(&target, header),
          Kind::Hardlink(_) => unreachable!("hard links are resolved above"),
        }
      }
    };
    pushed.map_err(|e| failed(format!("{}: {e}", at.display())))?;
  }
  let dir = path.parent().expect("a disk path has a directory");
  let mut staged = tempfile::NamedTempFile::new_in(dir).map_err(io(dir.display()))?;
  fs.write(staged.as_file_mut()).map_err(|e| failed(e.to_string()))?;
  staged.persist(path).map_err(|e| io(path.display())(e.error))?;
  Ok(())
}

/// Whites out, under the opaque directory `dir`, every entry `below` shows
/// that `tree` does not replace, and goes on into a directory `tree` keeps
/// over one below, which overlayfs would otherwise merge.
fn expand(tree: &mut BTreeMap<PathBuf, Node>, below: &Below, dir: &Path) {
  let children: Vec<(PathBuf, bool)> =
    below.descendants(dir).filter(|(path, _)| path.parent() == Some(dir)).map(|(path, is_dir)| (path.clone(), *is_dir)).collect();
  for (child, was_dir) in children {
    match tree.get(&child) {
      None => {
        tree.insert(child, Node::Whiteout);
      }
      Some(Node::Entry(entry)) if entry.is_dir() && was_dir => expand(tree, below, &child),
      Some(_) => {}
    }
  }
}

fn header(entry: &Entry) -> std::result::Result<backhand::NodeHeader, String> {
  let id = |id: u64, what: &str| u32::try_from(id).map_err(|_| format!("its {what} {id} does not fit SquashFS's 32 bits"));
  // NOTICE: SquashFS keeps 32-bit times; a later time is kept as the
  // latest it can hold.
  let mtime = u32::try_from(entry.mtime).unwrap_or(u32::MAX);
  Ok(backhand::NodeHeader::new((entry.mode & 0o7777) as u16, id(entry.uid, "uid")?, id(entry.gid, "gid")?, mtime))
}

/// A device number as SquashFS stores it: Linux's `new_encode_dev`, which
/// the kernel's SquashFS reader decodes with `new_decode_dev`.
// Source: https://github.com/torvalds/linux/blob/v6.18/include/linux/kdev_t.h
// (`new_encode_dev`); https://github.com/torvalds/linux/blob/v6.18/fs/squashfs/inode.c
// (`squashfs_read_inode`, `SQUASHFS_CHRDEV_TYPE`).
fn device_number(major: u32, minor: u32) -> u32 {
  (minor & 0xff) | (major << 8) | ((minor & !0xff) << 12)
}
