//! A layer's canonical archive: writing it from what a caller gives, and
//! reading it back as an index of its entries, whiteouts and opaque
//! directories.
//!
//! The canonical form is an OCI layer: an uncompressed tar whose deletions
//! are whiteout files, `.wh.NAME` for a removed entry and `.wh..wh..opq` in
//! a directory whose lower contents are hidden.
//! Reference: https://github.com/opencontainers/image-spec/blob/v1.1.1/layer.md#whiteouts
//!
//! An overlayfs upper directory marks the same things otherwise: a removed
//! entry is a character device 0/0, an opaque directory has the extended
//! attribute `trusted.overlay.opaque` (or `user.overlay.opaque` on a
//! `userxattr` mount) set to `y`.
//! Reference: https://github.com/torvalds/linux/blob/v6.18/Documentation/filesystems/overlayfs.rst
//! ("whiteouts and opaque directories")

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use super::{Error, Result, io};

/// The prefix of an OCI whiteout file's name.
const WHITEOUT: &str = ".wh.";
/// The name of an OCI opaque-directory marker.
const OPAQUE: &str = ".wh..wh..opq";

/// One entry of a layer, not a whiteout.
#[derive(Debug, Clone)]
pub(super) struct Entry {
  pub kind: Kind,
  pub mode: u32,
  pub uid: u64,
  pub gid: u64,
  pub mtime: u64,
}

impl Entry {
  pub fn is_dir(&self) -> bool {
    matches!(self.kind, Kind::Dir)
  }
}

#[derive(Debug, Clone)]
pub(super) enum Kind {
  File(Data),
  Dir,
  Symlink(PathBuf),
  /// A hard link to another entry of the same layer, by its path.
  Hardlink(PathBuf),
  Char(u32, u32),
  Block(u32, u32),
  Fifo,
}

/// Where a file's content is: a span of a stored archive.
#[derive(Debug, Clone)]
pub(super) struct Data {
  pub archive: Arc<Path>,
  pub offset: u64,
  pub size: u64,
}

impl Data {
  /// A reader of the content that opens the archive on its first read and
  /// closes it at the end, so a writer can hold one per file of a large
  /// layer without one open file each.
  pub fn reader(&self) -> Span {
    Span {
      data: self.clone(),
      file: None,
      left: self.size,
    }
  }
}

pub(super) struct Span {
  data: Data,
  file: Option<std::io::Take<File>>,
  left: u64,
}

impl Read for Span {
  fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
    if self.left == 0 {
      self.file = None;
      return Ok(0);
    }
    if self.file.is_none() {
      let mut file = File::open(&self.data.archive)?;
      file.seek(SeekFrom::Start(self.data.offset))?;
      self.file = Some(file.take(self.data.size));
    }
    let read = self.file.as_mut().expect("opened above").read(buf)?;
    if read == 0 {
      return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, format!("{} ends inside a file", self.data.archive.display())));
    }
    self.left -= read as u64;
    Ok(read)
  }
}

/// A layer, by path relative to its root: its entries, the paths its
/// whiteouts remove from the layers below, and its opaque directories,
/// whose contents in the layers below it hides.
#[derive(Debug, Clone, Default)]
pub(super) struct Index {
  pub entries: BTreeMap<PathBuf, Entry>,
  pub whiteouts: BTreeSet<PathBuf>,
  pub opaque: BTreeSet<PathBuf>,
}

impl Index {
  /// Reads the canonical archive at `path`, without its file contents.
  pub fn read(path: &Path) -> Result<Self> {
    let file = File::open(path).map_err(io(path.display()))?;
    let archive_path: Arc<Path> = Arc::from(path);
    let mut archive = tar::Archive::new(std::io::BufReader::new(file));
    let mut index = Self::default();
    let failed = |e: std::io::Error| io(path.display())(e);
    for entry in archive.entries_with_seek().map_err(failed)? {
      let entry = entry.map_err(failed)?;
      let Some(at) = clean(&entry.path().map_err(failed)?)? else {
        continue;
      };
      let name = at.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
      let parent = at.parent().map(Path::to_path_buf).unwrap_or_default();
      if name == OPAQUE {
        index.opaque.insert(parent);
        continue;
      }
      if let Some(removed) = name.strip_prefix(WHITEOUT) {
        index.whiteouts.insert(parent.join(removed));
        continue;
      }
      let header = entry.header();
      let kind = match header.entry_type() {
        tar::EntryType::Regular => Kind::File(Data {
          archive: archive_path.clone(),
          offset: entry.raw_file_position(),
          size: entry.size(),
        }),
        tar::EntryType::Directory => Kind::Dir,
        tar::EntryType::Symlink => Kind::Symlink(link(&entry)?),
        tar::EntryType::Link => Kind::Hardlink(clean(&link(&entry)?)?.unwrap_or_default()),
        tar::EntryType::Char => Kind::Char(device(header)?.0, device(header)?.1),
        tar::EntryType::Block => Kind::Block(device(header)?.0, device(header)?.1),
        tar::EntryType::Fifo => Kind::Fifo,
        other => return Err(Error::Invalid(format!("{}: {} is a {other:?} entry", path.display(), at.display()))),
      };
      index.entries.insert(at, metadata(header, kind)?);
    }
    Ok(index)
  }

  /// The entry at `at`; for a hard link, the entry it links to, whose
  /// metadata a hard link shares.
  pub fn resolve(&self, at: &Path) -> Result<Entry> {
    let mut path = at;
    // A chain of links is as long as the layer at most.
    for _ in 0..=self.entries.len() {
      let entry =
        self.entries.get(path).ok_or_else(|| Error::Invalid(format!("{} is a hard link to nothing in its layer", at.display())))?;
      match &entry.kind {
        Kind::Hardlink(target) => path = target,
        _ => return Ok(entry.clone()),
      }
    }
    Err(Error::Invalid(format!("{} is a hard link in a loop", at.display())))
  }

  /// Writes the layer as a canonical archive, in path order with hard links
  /// last, after the entries they link to.
  pub fn write(&self, out: &mut dyn Write) -> Result<()> {
    let mut builder = tar::Builder::new(out);
    let mut items: BTreeMap<PathBuf, Option<&Entry>> = BTreeMap::new();
    for (at, entry) in &self.entries {
      items.insert(at.clone(), Some(entry));
    }
    for removed in &self.whiteouts {
      let name = format!("{WHITEOUT}{}", removed.file_name().unwrap_or_default().to_string_lossy());
      items.insert(removed.parent().unwrap_or(Path::new("")).join(name), None);
    }
    for dir in &self.opaque {
      items.insert(dir.join(OPAQUE), None);
    }
    let (links, rest): (Vec<_>, Vec<_>) = items.into_iter().partition(|(_, entry)| {
      matches!(
        entry,
        Some(Entry {
          kind: Kind::Hardlink(_),
          ..
        })
      )
    });
    for (at, entry) in rest.into_iter().chain(links) {
      match entry {
        Some(entry) => append(&mut builder, &at, entry)?,
        None => append_whiteout(&mut builder, &at)?,
      }
    }
    builder.into_inner().map_err(io("writing a layer"))?;
    Ok(())
  }
}

/// Writes `layer`, an OCI layer or an overlayfs upper directory's archive,
/// to `out` in the canonical form: overlayfs whiteouts and opaque
/// directories become OCI whiteout files, paths are made relative without
/// `.` or `..`, the root's own entry is dropped, and every header is
/// written anew, which drops user and group names and extended attributes.
// NOTICE: entries keep the order they come in, so the digest is of this
// archive, not of a tree: two builds that write one tree in different orders
// make two layers.
// TODO(images): file capabilities and other extended attributes are not
// carried (the spec's "The layer store"); revisit with a disk writer that
// keeps them.
// NOTICE: an upper directory written with overlayfs's `redirect_dir` or
// `metacopy` features is not a whole layer; the guest mounts without them.
pub(super) fn canonical(layer: impl Read, out: &mut dyn Write) -> Result<()> {
  let mut archive = tar::Archive::new(layer);
  let mut builder = tar::Builder::new(out);
  let failed = |e: std::io::Error| Error::Invalid(e.to_string());
  for entry in archive.entries().map_err(failed)? {
    let mut entry = entry.map_err(failed)?;
    let opaque = is_opaque(&mut entry)?;
    let Some(at) = clean(&entry.path().map_err(failed)?)? else {
      continue;
    };
    let header = entry.header().clone();
    // An OCI whiteout file says only its name; its header is written as
    // an overlayfs whiteout's is, so both forms make one layer.
    if at.file_name().is_some_and(|name| name.to_string_lossy().starts_with(WHITEOUT)) {
      append_whiteout(&mut builder, &at)?;
      continue;
    }
    let kind = match header.entry_type() {
      tar::EntryType::Regular | tar::EntryType::Continuous => {
        let mut written = header_for(&header, tar::EntryType::Regular)?;
        written.set_size(entry.size());
        builder.append_data(&mut written, &at, &mut entry).map_err(io("writing a layer"))?;
        continue;
      }
      tar::EntryType::Directory => {
        let entry = metadata(&header, Kind::Dir)?;
        append(&mut builder, &at, &entry)?;
        if opaque {
          append_whiteout(&mut builder, &at.join(OPAQUE))?;
        }
        continue;
      }
      tar::EntryType::Char if device(&header)? == (0, 0) => {
        let name = format!("{WHITEOUT}{}", at.file_name().unwrap_or_default().to_string_lossy());
        append_whiteout(&mut builder, &at.parent().unwrap_or(Path::new("")).join(name))?;
        continue;
      }
      tar::EntryType::Symlink => Kind::Symlink(link(&entry)?),
      tar::EntryType::Link => {
        Kind::Hardlink(clean(&link(&entry)?)?.ok_or_else(|| Error::Invalid(format!("{} links to the root", at.display())))?)
      }
      tar::EntryType::Char => Kind::Char(device(&header)?.0, device(&header)?.1),
      tar::EntryType::Block => Kind::Block(device(&header)?.0, device(&header)?.1),
      tar::EntryType::Fifo => Kind::Fifo,
      tar::EntryType::XGlobalHeader => continue,
      other => return Err(Error::Invalid(format!("{} is a {other:?} entry", at.display()))),
    };
    append(&mut builder, &at, &metadata(&header, kind)?)?;
  }
  builder.into_inner().map_err(io("writing a layer"))?;
  Ok(())
}

/// Whether an upper directory's entry is an opaque directory, by its
/// extended attribute in the entry's PAX header.
fn is_opaque<R: Read>(entry: &mut tar::Entry<'_, R>) -> Result<bool> {
  // Reference: https://www.gnu.org/software/tar/manual/html_node/Extensions.html
  // (`SCHILY.xattr.NAME`), the form GNU tar and the `tar` crate write
  // extended attributes in.
  let Some(extensions) = entry.pax_extensions().map_err(|e| Error::Invalid(e.to_string()))? else {
    return Ok(false);
  };
  for extension in extensions {
    let extension = extension.map_err(|e| Error::Invalid(e.to_string()))?;
    let key = extension.key().map_err(|e| Error::Invalid(e.to_string()))?;
    if matches!(key, "SCHILY.xattr.trusted.overlay.opaque" | "SCHILY.xattr.user.overlay.opaque") && extension.value_bytes() == b"y" {
      return Ok(true);
    }
  }
  Ok(false)
}

/// `path` relative to the layer's root, or `None` for the root itself.
/// Refuses a path that climbs out with `..`.
fn clean(path: &Path) -> Result<Option<PathBuf>> {
  let mut clean = PathBuf::new();
  for component in path.components() {
    match component {
      Component::Normal(part) => clean.push(part),
      Component::RootDir | Component::CurDir => {}
      Component::ParentDir | Component::Prefix(_) => {
        return Err(Error::Invalid(format!("{} leaves the layer", path.display())));
      }
    }
  }
  Ok((!clean.as_os_str().is_empty()).then_some(clean))
}

fn link<R: Read>(entry: &tar::Entry<'_, R>) -> Result<PathBuf> {
  entry
    .link_name()
    .map_err(|e| Error::Invalid(e.to_string()))?
    .map(|name| name.into_owned())
    .ok_or_else(|| Error::Invalid(format!("a link without a target in {}", String::from_utf8_lossy(&entry.path_bytes()))))
}

fn device(header: &tar::Header) -> Result<(u32, u32)> {
  let invalid = |e: std::io::Error| Error::Invalid(e.to_string());
  Ok((header.device_major().map_err(invalid)?.unwrap_or(0), header.device_minor().map_err(invalid)?.unwrap_or(0)))
}

fn metadata(header: &tar::Header, kind: Kind) -> Result<Entry> {
  let invalid = |e: std::io::Error| Error::Invalid(e.to_string());
  Ok(Entry {
    kind,
    mode: header.mode().map_err(invalid)? & 0o7777,
    uid: header.uid().map_err(invalid)?,
    gid: header.gid().map_err(invalid)?,
    mtime: header.mtime().map_err(invalid)?,
  })
}

/// A fresh GNU header of `kind` with `source`'s mode, owner and time.
fn header_for(source: &tar::Header, kind: tar::EntryType) -> Result<tar::Header> {
  // The kind is the header's own; only the entry's metadata is used.
  Ok(header(&metadata(source, Kind::Dir)?, kind))
}

fn header(entry: &Entry, kind: tar::EntryType) -> tar::Header {
  let mut header = tar::Header::new_gnu();
  header.set_entry_type(kind);
  header.set_mode(entry.mode);
  header.set_uid(entry.uid);
  header.set_gid(entry.gid);
  header.set_mtime(entry.mtime);
  header.set_size(0);
  header
}

fn append<W: Write>(builder: &mut tar::Builder<W>, at: &Path, entry: &Entry) -> Result<()> {
  let failed = io("writing a layer");
  match &entry.kind {
    Kind::File(data) => {
      let mut written = header(entry, tar::EntryType::Regular);
      written.set_size(data.size);
      builder.append_data(&mut written, at, data.reader()).map_err(failed)
    }
    Kind::Dir => builder.append_data(&mut header(entry, tar::EntryType::Directory), at, std::io::empty()).map_err(failed),
    Kind::Symlink(target) => builder.append_link(&mut header(entry, tar::EntryType::Symlink), at, target).map_err(failed),
    Kind::Hardlink(target) => builder.append_link(&mut header(entry, tar::EntryType::Link), at, target).map_err(failed),
    Kind::Char(major, minor) | Kind::Block(major, minor) => {
      let kind = if matches!(entry.kind, Kind::Char(..)) {
        tar::EntryType::Char
      } else {
        tar::EntryType::Block
      };
      let mut written = header(entry, kind);
      written.set_device_major(*major).map_err(failed_device)?;
      written.set_device_minor(*minor).map_err(failed_device)?;
      builder.append_data(&mut written, at, std::io::empty()).map_err(failed)
    }
    Kind::Fifo => builder.append_data(&mut header(entry, tar::EntryType::Fifo), at, std::io::empty()).map_err(failed),
  }
}

fn failed_device(e: std::io::Error) -> Error {
  Error::Invalid(e.to_string())
}

/// An OCI whiteout file at `at`: empty, root's, mode 0, time 0.
fn append_whiteout<W: Write>(builder: &mut tar::Builder<W>, at: &Path) -> Result<()> {
  let mut header = tar::Header::new_gnu();
  header.set_entry_type(tar::EntryType::Regular);
  header.set_mode(0);
  header.set_uid(0);
  header.set_gid(0);
  header.set_mtime(0);
  header.set_size(0);
  builder.append_data(&mut header, at, std::io::empty()).map_err(io("writing a layer"))
}
