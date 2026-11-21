//! The layer store: an image's layers by content, the snapshots they stack
//! into, an image's commits, and the disks a VM driver boots a snapshot
//! from.
//!
//! A layer's one canonical form is an OCI layer archive (an uncompressed
//! tar, deletions as `.wh.` whiteouts), named by the SHA-256 of that
//! archive, its OCI `DiffID`. What a driver boots is derived from it and
//! cached here: [`Store::materialize`] makes SquashFS layer disks for the VM
//! drivers, and [`Store::materialize_container`] assembles a loadable
//! Docker/OCI image archive for the container drivers. No caller sees
//! whiteouts, disk formats or how commits collapse.
//!
//! Built layers, snapshot records, disks and OCI archives are cache, in the
//! cache directory: they can be built again. An image's commits are a user's
//! hand edits, which cannot, so its commit layers and history are in the data
//! directory. [`Store::collect`] removes what is named neither by the caller
//! nor by a commit history.
//!
//! Everything here is blocking file IO, some of it on whole layers: call it
//! off the async runtime.
//!
//! See `docs/ai/references/architecture/2026-11-17-images-and-run-data-spec.md`
//! ("The layer store").
// NOTICE: the directory layout (`layers/`, `snapshots/`, `disks/`, `oci/`,
// `images/`, each file `sha256-<hex>.<ext>`) is provisional and private to
// this module; nothing outside reads it.
// TODO(images): a general listing of stored layers waits for `needy image ls`
// or `log` to need it.

mod archive;
mod collapse;
mod disk;
mod oci;

pub use oci::{extract_top_layer, snapshot_tag};

use std::collections::HashSet;
use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use sha2::Digest as _;

/// What the store refused or could not do.
#[derive(Debug, thiserror::Error)]
pub enum Error {
  /// No layer or snapshot with this digest is stored.
  #[error("no layer or snapshot {0} in the layer store")]
  Missing(Digest),
  /// A commit the caller named is not one of the image's commits.
  #[error("{digest} is not a commit of the image {image}")]
  UnknownCommit { image: String, digest: Digest },
  /// A rollback went further back than the image's commits go.
  #[error("cannot roll the image {image} back {steps} commits: it has {available}")]
  TooFar {
    image: String,
    steps: usize,
    available: usize,
  },
  /// A layer archive the store cannot take.
  #[error("not a layer the store can take: {0}")]
  Invalid(String),
  /// A file of the store could not be read or written.
  #[error("{context}: {source}")]
  Io {
    context: String,
    #[source]
    source: std::io::Error,
  },
}

type Result<T> = std::result::Result<T, Error>;

fn io(context: impl fmt::Display) -> impl FnOnce(std::io::Error) -> Error {
  move |source| Error::Io {
    context: context.to_string(),
    source,
  }
}

/// A SHA-256 digest: of a layer's canonical archive, or of a snapshot's
/// stack. Written `sha256:<64 hex digits>`, as OCI writes digests.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest([u8; 32]);

impl Digest {
  /// The digest of `bytes`.
  pub fn of(bytes: &[u8]) -> Self {
    Self(sha2::Sha256::digest(bytes).into())
  }

  fn hex(&self) -> String {
    use std::fmt::Write as _;
    self.0.iter().fold(String::with_capacity(64), |mut hex, byte| {
      // NOTICE: writing to a `String` cannot fail.
      let _ = write!(hex, "{byte:02x}");
      hex
    })
  }

  /// The name of this digest's files in the store: `sha256-<hex>`, since a
  /// `:` is not portable in file names.
  fn stem(&self) -> String {
    format!("sha256-{}", self.hex())
  }

  fn from_stem(stem: &str) -> Option<Self> {
    stem.strip_prefix("sha256-").and_then(|hex| format!("sha256:{hex}").parse().ok())
  }
}

impl fmt::Display for Digest {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "sha256:{}", self.hex())
  }
}

impl fmt::Debug for Digest {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    fmt::Display::fmt(self, f)
  }
}

impl FromStr for Digest {
  type Err = Error;

  fn from_str(text: &str) -> Result<Self> {
    let invalid = || Error::Invalid(format!("{text:?} is not a digest (sha256:<64 hex digits>)"));
    let hex = text.strip_prefix("sha256:").filter(|hex| hex.len() == 64).ok_or_else(invalid)?;
    let mut bytes = [0; 32];
    for (byte, pair) in bytes.iter_mut().zip(hex.as_bytes().chunks(2)) {
      let pair = std::str::from_utf8(pair).map_err(|_| invalid())?;
      *byte = u8::from_str_radix(pair, 16).map_err(|_| invalid())?;
    }
    Ok(Self(bytes))
  }
}

impl serde::Serialize for Digest {
  fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
    serializer.collect_str(self)
  }
}

impl<'de> serde::Deserialize<'de> for Digest {
  fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
    let text = String::deserialize(deserializer)?;
    text.parse().map_err(serde::de::Error::custom)
  }
}

/// A stack of layers, lowest first, named by the digest of the stack: what
/// a run boots. Immutable.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Snapshot {
  pub digest: Digest,
  pub layers: Vec<Digest>,
}

/// One read-only disk of a layer, for a VM driver to attach. The disk is a
/// SquashFS image with the layer's tree under [`crate::contract::LAYER_ROOT`]
/// and its digest in [`crate::contract::LAYER_MARKER`], by which the guest
/// finds it whatever order the disks are attached in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerDisk {
  pub digest: Digest,
  pub path: PathBuf,
}

/// A program an edit session ran, as the trace recorded it, kept with the
/// commit so the commit stays explainable. Provisional: the shape the daemon
/// records (slice 1, step 7) may add fields.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Command {
  pub argv: Vec<String>,
  pub cwd: String,
}

impl Command {
  pub fn new(argv: Vec<String>, cwd: impl Into<String>) -> Self {
    Self {
      argv,
      cwd: cwd.into(),
    }
  }
}

/// One commit of an image: the commit layer it made, which holds every
/// commit before it too, the commit it was made on, and what its session
/// ran.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Commit {
  pub layer: Digest,
  pub parent: Option<Digest>,
  pub commands: Vec<Command>,
  /// When it was made, in milliseconds since the Unix epoch.
  pub time_ms: i64,
}

/// What [`Store::collect`] removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Collected {
  /// Layers removed.
  pub layers: usize,
  /// Bytes freed: the layers, their disks and snapshot records.
  pub bytes: u64,
}

/// An image's commit history, in the data directory.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct History {
  image: String,
  commits: Vec<Commit>,
}

/// The layer store over a cache and a data directory, which it creates when
/// it first writes to them.
#[derive(Debug, Clone)]
pub struct Store {
  cache: PathBuf,
  data: PathBuf,
}

impl Store {
  /// A store keeping what it can rebuild in `cache` and an image's commits
  /// in `data`.
  pub fn new(cache: impl Into<PathBuf>, data: impl Into<PathBuf>) -> Self {
    Self {
      cache: cache.into(),
      data: data.into(),
    }
  }

  /// Stores a built layer from `layer`, an uncompressed layer archive, and
  /// returns its digest. The archive is an OCI layer, or an overlayfs upper
  /// directory as a session archives it (whiteouts as character devices
  /// 0/0, an opaque directory by its `trusted.overlay.opaque` or
  /// `user.overlay.opaque` attribute in a PAX header); either is kept in
  /// the canonical OCI form.
  // TODO(images): compressed layers (gzip, zstd), as a registry serves a
  // root's, are decompressed by the caller until pulling a root needs it.
  pub fn put(&self, layer: impl Read) -> Result<Digest> {
    self.store(layer, &self.cache.join("layers"))
  }

  /// Stores the top layer of a committed container image archive (Docker save
  /// or OCI image layout) as a canonical layer in the store and returns its
  /// digest.
  pub fn put_from_image_export(&self, image_tar: &Path) -> Result<Digest> {
    let top = oci::extract_top_layer(image_tar)?;
    self.put(top)
  }

  /// Whether a layer with `digest` is stored (in either the cache or the
  /// data directory).
  pub fn has_layer(&self, digest: &Digest) -> bool {
    self.layer(digest).is_ok()
  }

  /// Records the stack `layers`, lowest first, as a snapshot. Every layer
  /// must be stored.
  pub fn snapshot(&self, layers: &[Digest]) -> Result<Snapshot> {
    for layer in layers {
      self.layer(layer)?;
    }
    let snapshot = Snapshot {
      digest: stack_digest(layers),
      layers: layers.to_vec(),
    };
    let path = self.snapshot_path(&snapshot.digest);
    if !path.exists() {
      let json = serde_json::to_vec_pretty(&snapshot).expect("a snapshot serializes");
      write_atomic(&path, &json)?;
    }
    Ok(snapshot)
  }

  /// The snapshot recorded under `digest`.
  pub fn resolve(&self, digest: &Digest) -> Result<Snapshot> {
    let path = self.snapshot_path(digest);
    let json = match std::fs::read(&path) {
      Ok(json) => json,
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Error::Missing(*digest)),
      Err(e) => return Err(io(path.display())(e)),
    };
    serde_json::from_slice(&json).map_err(|e| Error::Invalid(format!("{}: {e}", path.display())))
  }

  /// The disks a VM driver attaches to boot `snapshot`, one per layer, in
  /// the snapshot's order. Each is made the first time it is asked for and
  /// reused after. A layer with opaque directories gets a disk per stack
  /// below it, since the whiteouts it is given depend on what is below.
  pub fn materialize(&self, snapshot: &Snapshot) -> Result<Vec<LayerDisk>> {
    let dir = self.cache.join("disks");
    let indexes = snapshot.layers.iter().map(|digest| archive::Index::read(&self.layer(digest)?)).collect::<Result<Vec<_>>>()?;
    // What the layers below show is tracked only as far as a layer that
    // needs it.
    let needed = indexes.iter().rposition(disk::hides_below).unwrap_or(0);
    let mut below = disk::Below::default();
    let mut disks = Vec::with_capacity(snapshot.layers.len());
    for (at, (digest, index)) in snapshot.layers.iter().zip(&indexes).enumerate() {
      let name = if disk::hides_below(index) {
        format!("{}-on-{}.squashfs", digest.stem(), &stack_digest(&snapshot.layers[..at]).hex()[..16])
      } else {
        format!("{}.squashfs", digest.stem())
      };
      let path = dir.join(name);
      if !path.exists() {
        std::fs::create_dir_all(&dir).map_err(io(dir.display()))?;
        disk::write(index, digest, &below, &path)?;
      }
      if at < needed {
        below.stack(index);
      }
      disks.push(LayerDisk {
        digest: *digest,
        path,
      });
    }
    Ok(disks)
  }

  /// Assembles a `docker load` / `container image load`-compatible OCI image
  /// archive for `snapshot` layered on `base_image_tar`, tagged as `tag`.
  ///
  /// Appends each layer of `snapshot` in order, runs the in-place shell shim
  /// step whenever a snapshot layer adds or replaces a shell binary, and
  /// caches the resulting archive under the store's cache directory keyed by
  /// the snapshot digest, the base image config digest, and `tag`.
  pub fn materialize_container(&self, snapshot: &Snapshot, base_image_tar: &Path, tag: &str) -> Result<PathBuf> {
    let layers: Vec<(Digest, PathBuf)> = snapshot.layers.iter().map(|digest| Ok((*digest, self.layer(digest)?))).collect::<Result<_>>()?;
    oci::materialize(&self.cache.join("oci"), snapshot.digest, &layers, base_image_tar, tag)
  }

  /// Commits an edit session's upper layer `upper` (as [`Store::put`] takes
  /// it) to `image`, on its commit `parent`, or as its first commit: the
  /// edit is collapsed into the parent's commit layer, so an image has one
  /// commit layer however many edits it had. Returns the new commit layer.
  ///
  /// `image` is the caller's name for the image; the store keys its history
  /// by it.
  // NOTICE: one edit session per image at a time (the spec's "Building and
  // editing"), which the daemon enforces; two commits racing on one image
  // would each rewrite its history.
  // NOTICE: the daemon does not run `collect` while a commit is being made:
  // the new commit layer is stored before the history names it, and
  // `collect` removes a commit layer no history names.
  pub fn commit(&self, image: &str, parent: Option<&Digest>, upper: impl Read, commands: Vec<Command>) -> Result<Digest> {
    let mut history = self.read_history(image)?;
    let layers = self.data.join("layers");
    let layer = match parent {
      None => self.store(upper, &layers)?,
      Some(parent) => {
        if !history.commits.iter().any(|commit| commit.layer == *parent) {
          return Err(Error::UnknownCommit {
            image: image.to_string(),
            digest: *parent,
          });
        }
        // The edit on its own is never a commit layer: it is staged, read
        // and dropped.
        std::fs::create_dir_all(&layers).map_err(io(layers.display()))?;
        let staged = tempfile::NamedTempFile::new_in(&layers).map_err(io(layers.display()))?;
        let mut out = std::io::BufWriter::new(staged.as_file());
        archive::canonical(upper, &mut out)?;
        out.flush().map_err(io(staged.path().display()))?;
        drop(out);
        let lower = archive::Index::read(&self.layer(parent)?)?;
        let edit = archive::Index::read(staged.path())?;
        let merged = collapse::collapse(&lower, &edit)?;
        self.store_with(&layers, |out| merged.write(out))?
      }
    };
    history.image = image.to_string();
    history.commits.push(Commit {
      layer,
      parent: parent.copied(),
      commands,
      time_ms: needy_common::time::now_ms(),
    });
    let json = serde_json::to_vec_pretty(&history).expect("a history serializes");
    write_atomic(&self.history_path(image), &json)?;
    Ok(layer)
  }

  /// The commits made to `image`, oldest first; empty when it has none.
  pub fn history(&self, image: &str) -> Result<Vec<Commit>> {
    Ok(self.read_history(image)?.commits)
  }

  /// The commit layer `image` has `steps` commits before its commit `from`:
  /// `None` when that is before its first commit. The history keeps every
  /// commit, so a rolled-back one can still be booted.
  pub fn rollback(&self, image: &str, from: &Digest, steps: usize) -> Result<Option<Digest>> {
    let history = self.read_history(image)?;
    let find = |layer: &Digest| history.commits.iter().rev().find(|commit| commit.layer == *layer);
    let mut at = find(from).ok_or_else(|| Error::UnknownCommit {
      image: image.to_string(),
      digest: *from,
    })?;
    let mut chain = vec![at.layer];
    while let Some(parent) = at.parent.as_ref().and_then(find) {
      chain.push(parent.layer);
      at = parent;
    }
    if steps > chain.len() {
      return Err(Error::TooFar {
        image: image.to_string(),
        steps,
        available: chain.len(),
      });
    }
    Ok(chain.get(steps).copied())
  }

  /// Removes the built layers, snapshot records and disks that neither
  /// `keep` nor a commit history names. `keep` holds what the caller's
  /// live runs and lockfiles name: snapshots (with every layer in them) or
  /// layers. A commit layer that no history names (a commit cut short) is
  /// removed too.
  // NOTICE: files that are not `sha256-<hex>.<ext>` are left alone: they are
  // a write in progress (`tempfile`'s `.tmp` names) or not the store's.
  pub fn collect(&self, keep: &[Digest]) -> Result<Collected> {
    let mut kept: HashSet<Digest> = keep.iter().copied().collect();
    for digest in keep {
      match self.resolve(digest) {
        Ok(snapshot) => kept.extend(snapshot.layers),
        Err(Error::Missing(_)) => {}
        Err(e) => return Err(e),
      }
    }
    let committed = self.committed()?;
    let mut collected = Collected::default();
    for (dir, is_layer) in [
      (self.cache.join("layers"), true),
      (self.data.join("layers"), true),
      (self.cache.join("disks"), false),
    ] {
      for (path, digest) in stored(&dir)? {
        if !kept.contains(&digest) && !committed.contains(&digest) {
          collected.bytes += remove_file(&path)?;
          collected.layers += usize::from(is_layer);
        }
      }
    }
    for (path, digest) in stored(&self.cache.join("snapshots"))?.into_iter().chain(stored(&self.cache.join("oci"))?) {
      if !kept.contains(&digest) {
        collected.bytes += remove_file(&path)?;
      }
    }
    Ok(collected)
  }

  /// The archive of a stored layer: a built layer in the cache, or a commit
  /// layer in the data directory.
  fn layer(&self, digest: &Digest) -> Result<PathBuf> {
    let name = format!("{}.tar", digest.stem());
    [
      self.cache.join("layers").join(&name),
      self.data.join("layers").join(&name),
    ]
    .into_iter()
    .find(|path| path.exists())
    .ok_or(Error::Missing(*digest))
  }

  /// Stores the canonical form of the layer archive `layer` in `dir`.
  fn store(&self, layer: impl Read, dir: &Path) -> Result<Digest> {
    self.store_with(dir, |out| archive::canonical(layer, out))
  }

  /// Stores in `dir` the archive `write` writes, named by its digest.
  fn store_with(&self, dir: &Path, write: impl FnOnce(&mut dyn Write) -> Result<()>) -> Result<Digest> {
    std::fs::create_dir_all(dir).map_err(io(dir.display()))?;
    let staged = tempfile::NamedTempFile::new_in(dir).map_err(io(dir.display()))?;
    let mut out = Hashing {
      inner: std::io::BufWriter::new(staged.as_file().try_clone().map_err(io(staged.path().display()))?),
      hasher: sha2::Sha256::new(),
    };
    write(&mut out)?;
    let Hashing { mut inner, hasher } = out;
    inner.flush().map_err(io(staged.path().display()))?;
    drop(inner);
    let digest = Digest(hasher.finalize().into());
    let path = dir.join(format!("{}.tar", digest.stem()));
    if !path.exists() {
      staged.persist(&path).map_err(|e| io(path.display())(e.error))?;
    }
    Ok(digest)
  }

  fn snapshot_path(&self, digest: &Digest) -> PathBuf {
    self.cache.join("snapshots").join(format!("{}.json", digest.stem()))
  }

  /// An image's history file, in a directory named by the digest of its
  /// name, which may be any text.
  fn history_path(&self, image: &str) -> PathBuf {
    self.data.join("images").join(&Digest::of(image.as_bytes()).hex()[..32]).join("history.json")
  }

  fn read_history(&self, image: &str) -> Result<History> {
    let path = self.history_path(image);
    match std::fs::read(&path) {
      Ok(json) => serde_json::from_slice(&json).map_err(|e| Error::Invalid(format!("{}: {e}", path.display()))),
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(History::default()),
      Err(e) => Err(io(path.display())(e)),
    }
  }

  /// Every layer an image's history names.
  fn committed(&self) -> Result<HashSet<Digest>> {
    let dir = self.data.join("images");
    let entries = match std::fs::read_dir(&dir) {
      Ok(entries) => entries,
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashSet::new()),
      Err(e) => return Err(io(dir.display())(e)),
    };
    let mut committed = HashSet::new();
    for entry in entries {
      let entry = entry.map_err(io(dir.display()))?;
      let path = entry.path().join("history.json");
      let json = match std::fs::read(&path) {
        Ok(json) => json,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
        Err(e) => return Err(io(path.display())(e)),
      };
      let history: History = serde_json::from_slice(&json).map_err(|e| Error::Invalid(format!("{}: {e}", path.display())))?;
      committed.extend(history.commits.iter().map(|commit| commit.layer));
    }
    Ok(committed)
  }
}

/// The digest of a stack of layers: of each layer's digest on its own line,
/// lowest first.
fn stack_digest(layers: &[Digest]) -> Digest {
  let text: String = layers.iter().map(|layer| format!("{layer}\n")).collect();
  Digest::of(text.as_bytes())
}

/// The store's files in `dir` with the digest each is named by; none when
/// `dir` does not exist.
fn stored(dir: &Path) -> Result<Vec<(PathBuf, Digest)>> {
  let entries = match std::fs::read_dir(dir) {
    Ok(entries) => entries,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
    Err(e) => return Err(io(dir.display())(e)),
  };
  let mut files = Vec::new();
  for entry in entries {
    let path = entry.map_err(io(dir.display()))?.path();
    // `sha256-<hex>.tar`, `.json`, `.squashfs`, or a disk's
    // `sha256-<hex>-on-<stack>.squashfs`.
    let digest = path.file_name().and_then(|name| name.to_str()).and_then(|name| name.get(..7 + 64)).and_then(Digest::from_stem);
    if let Some(digest) = digest {
      files.push((path, digest));
    }
  }
  Ok(files)
}

/// Removes `path` and returns its size.
fn remove_file(path: &Path) -> Result<u64> {
  let size = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
  std::fs::remove_file(path).map_err(io(path.display()))?;
  Ok(size)
}

/// Writes `bytes` to `path` whole or not at all: beside it, then renamed.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
  let dir = path.parent().expect("a store path has a directory");
  std::fs::create_dir_all(dir).map_err(io(dir.display()))?;
  let mut staged = tempfile::NamedTempFile::new_in(dir).map_err(io(dir.display()))?;
  staged.write_all(bytes).map_err(io(path.display()))?;
  staged.persist(path).map_err(|e| io(path.display())(e.error))?;
  Ok(())
}

/// A writer that hashes what it writes.
struct Hashing<W> {
  inner: W,
  hasher: sha2::Sha256,
}

impl<W: Write> Write for Hashing<W> {
  fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
    let written = self.inner.write(bytes)?;
    self.hasher.update(&bytes[..written]);
    Ok(written)
  }

  fn flush(&mut self) -> std::io::Result<()> {
    self.inner.flush()
  }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
