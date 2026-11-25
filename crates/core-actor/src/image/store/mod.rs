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
//! [`import`] takes the layers of an OCI image built `FROM` needy's
//! `actor-base` that are above the base, and [`registry`] shares built steps
//! through an OCI registry.
//!
//! Generic OCI layer canonicalization, algebraic collapse, SquashFS disk
//! writing, and OCI/Docker hybrid archive assembly live in the standalone
//! `oci-layer` crate; this module owns needy's cache/data store layout,
//! commit history, and container shell-shim layer synthesis.
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

pub mod import;
mod oci;
pub mod registry;

pub use needy_core::image::Digest;
pub use oci::{extract_top_layer, snapshot_tag};

use std::collections::HashSet;
use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use oci_layer::archive;
use oci_layer::collapse;
use oci_layer::disk::{self, DiskLayout};
use sha2::Digest as _;

use crate::contract::{LAYER_MARKER, LAYER_ROOT};

/// Maximum number of individual SquashFS layer disks [`Store::materialize`]
/// returns for a VM snapshot before collapsing the lower prefix into one disk.
// NOTICE: measured 8-layer `macos-vz` boot overhead is +84 ms, and Firecracker
// caps virtio-mmio block devices at 16 (leaving room for rootfs, guest disk,
// and identity disks). Keeping the top `MAX_UNFLATTENED_LAYERS - 1` layers
// individual lets the top edit commit and recent layers swap without repacking
// the collapsed lower disk.
pub const MAX_UNFLATTENED_LAYERS: usize = 8;

const NEEDY_DISK_LAYOUT: DiskLayout<'static> = DiskLayout {
  marker: LAYER_MARKER,
  root: LAYER_ROOT,
};

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
  /// A layer archive is another layer than the one it was taken as.
  #[error("the layer archive is {actual}, not the expected {expected}")]
  Unexpected { expected: Digest, actual: Digest },
  /// A file of the store could not be read or written.
  #[error("{context}: {source}")]
  Io {
    context: String,
    #[source]
    source: std::io::Error,
  },
}

impl From<oci_layer::Error> for Error {
  fn from(err: oci_layer::Error) -> Self {
    match err {
      oci_layer::Error::Invalid(msg) => Self::Invalid(msg),
      oci_layer::Error::Io { context, source } => Self::Io { context, source },
    }
  }
}

type Result<T> = std::result::Result<T, Error>;

fn io(context: impl fmt::Display) -> impl FnOnce(std::io::Error) -> Error {
  move |source| Error::Io {
    context: context.to_string(),
    source,
  }
}

pub(super) fn to_oci_digest(digest: Digest) -> oci_layer::Digest {
  oci_layer::Digest::from_bytes(*digest.as_bytes())
}

/// The name of this digest's files in the store: `sha256-<hex>`, since a
/// `:` is not portable in file names.
fn stem(digest: &Digest) -> String {
  format!("sha256-{}", digest.hex())
}

fn from_stem(stem: &str) -> Option<Digest> {
  stem.strip_prefix("sha256-").and_then(|hex| Digest::parse(&format!("sha256:{hex}")).ok())
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
/// commit before it too, the commit it was made on, the build plan top key it
/// was made on, and what its session ran.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Commit {
  pub layer: Digest,
  pub parent: Option<Digest>,
  /// The build plan top digest (`Plan::top`) this commit was made on, if
  /// recorded (absent in histories written before base persistence).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub base: Option<Digest>,
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

  /// Opens the canonical archive of stored layer `digest`, the uncompressed
  /// OCI layer whose SHA-256 is `digest`, as a remote actor runner fetches
  /// it. Storing it again with [`Store::put`] gives the same digest.
  pub fn open_layer(&self, digest: &Digest) -> Result<std::fs::File> {
    let path = self.layer(digest)?;
    std::fs::File::open(&path).map_err(io(path.display()))
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

  /// The disks a VM driver attaches to boot `snapshot`, in the snapshot's
  /// order. Each is made the first time it is asked for and reused after.
  ///
  /// When `snapshot.layers.len() > MAX_UNFLATTENED_LAYERS`, the lower prefix
  /// (`0..layers.len() - (MAX_UNFLATTENED_LAYERS - 1)`) is collapsed into a
  /// single cached SquashFS disk keyed by the prefix's stack digest, leaving
  /// the top `MAX_UNFLATTENED_LAYERS - 1` layers individual so the returned
  /// disk count never exceeds [`MAX_UNFLATTENED_LAYERS`]. A layer with opaque
  /// directories gets a disk per stack below it, since the whiteouts it is
  /// given depend on what is below.
  pub fn materialize(&self, snapshot: &Snapshot) -> Result<Vec<LayerDisk>> {
    let dir = self.cache.join("disks");
    let (start_at, flat_spec) = if snapshot.layers.len() > MAX_UNFLATTENED_LAYERS {
      let prefix_len = snapshot.layers.len() - (MAX_UNFLATTENED_LAYERS - 1);
      let prefix_digest = stack_digest(&snapshot.layers[..prefix_len]);
      let path = dir.join(format!("{}-flat.squashfs", stem(&prefix_digest)));
      (prefix_len, Some((prefix_digest, path)))
    } else {
      (0, None)
    };

    // Probe cached disks before parsing any layer tar archives: a layer's
    // `hides_below` property is a pure function of its content, so a layer
    // produces either `<stem>.squashfs` or `<stem>-on-<stack>.squashfs`, never
    // both.
    let flat_hit = flat_spec.as_ref().is_none_or(|(_, path)| path.exists());
    let mut cached_upper: Vec<Option<PathBuf>> = Vec::with_capacity(snapshot.layers.len() - start_at);
    let mut all_upper_hit = true;
    for (at, digest) in snapshot.layers.iter().enumerate().skip(start_at) {
      let plain = dir.join(format!("{}.squashfs", stem(digest)));
      let found = if plain.exists() {
        Some(plain)
      } else {
        let on_stack = dir.join(format!("{}-on-{}.squashfs", stem(digest), &stack_digest(&snapshot.layers[..at]).hex()[..16]));
        on_stack.exists().then_some(on_stack)
      };
      if found.is_none() {
        all_upper_hit = false;
      }
      cached_upper.push(found);
    }

    if flat_hit && all_upper_hit {
      let mut disks = Vec::with_capacity(usize::from(flat_spec.is_some()) + cached_upper.len());
      if let Some((digest, path)) = flat_spec {
        disks.push(LayerDisk { digest, path });
      }
      for (digest, path) in snapshot.layers.iter().skip(start_at).zip(cached_upper) {
        disks.push(LayerDisk {
          digest: *digest,
          path: path.expect("all_upper_hit guarantees Some"),
        });
      }
      return Ok(disks);
    }

    // Lazily read layer tar indexes only for layers whose disk is missing,
    // plus lower layers needed to collapse a missing prefix or expand whiteouts
    // for a missing opaque layer.
    let mut indexes: Vec<Option<archive::Index>> = vec![None; snapshot.layers.len()];
    let load_index = |indexes: &mut [Option<archive::Index>], idx: usize| -> Result<()> {
      if indexes[idx].is_none() {
        indexes[idx] = Some(archive::Index::read(&self.layer(&snapshot.layers[idx])?)?);
      }
      Ok(())
    };

    let mut disks = Vec::with_capacity(usize::from(flat_spec.is_some()) + cached_upper.len());
    if let Some((prefix_digest, path)) = flat_spec {
      if !flat_hit {
        for i in 0..start_at {
          load_index(&mut indexes, i)?;
        }
        let mut merged = indexes[0].as_ref().expect("loaded above").clone();
        for upper in &indexes[1..start_at] {
          merged = collapse::collapse(&merged, upper.as_ref().expect("loaded above"))?;
        }
        std::fs::create_dir_all(&dir).map_err(io(dir.display()))?;
        disk::write(&merged, &to_oci_digest(prefix_digest), &disk::Below::default(), &NEEDY_DISK_LAYOUT, &path)?;
      }
      disks.push(LayerDisk {
        digest: prefix_digest,
        path,
      });
    }

    // Load missing upper layers to determine whether any of them hides lower
    // entries (and thus requires stacking `below` up to that position).
    let mut needed: Option<usize> = None;
    for (offset, cached) in cached_upper.iter().enumerate() {
      if cached.is_none() {
        let at = start_at + offset;
        load_index(&mut indexes, at)?;
        if disk::hides_below(indexes[at].as_ref().expect("loaded above")) {
          needed = Some(at);
        }
      }
    }

    let mut below = disk::Below::default();
    if needed.is_some() {
      for i in 0..start_at {
        load_index(&mut indexes, i)?;
        below.stack(indexes[i].as_ref().expect("loaded above"));
      }
    }

    for (offset, cached) in cached_upper.into_iter().enumerate() {
      let at = start_at + offset;
      let digest = &snapshot.layers[at];
      let path = match cached {
        Some(path) => path,
        None => {
          let index = indexes[at].as_ref().expect("missing upper layer loaded above");
          let name = if disk::hides_below(index) {
            format!("{}-on-{}.squashfs", stem(digest), &stack_digest(&snapshot.layers[..at]).hex()[..16])
          } else {
            format!("{}.squashfs", stem(digest))
          };
          let path = dir.join(name);
          if !path.exists() {
            std::fs::create_dir_all(&dir).map_err(io(dir.display()))?;
            disk::write(index, &to_oci_digest(*digest), &below, &NEEDY_DISK_LAYOUT, &path)?;
          }
          path
        }
      };
      if needed.is_some_and(|max| at < max) {
        load_index(&mut indexes, at)?;
        below.stack(indexes[at].as_ref().expect("loaded above"));
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

  /// Assembles a standalone `docker load` / `container image load`-compatible
  /// OCI archive containing `snapshot`'s layers, tagged as `tag`, without
  /// requiring a container base image tar.
  pub fn export_snapshot(&self, snapshot: &Snapshot, tag: &str) -> Result<PathBuf> {
    let layers: Vec<(Digest, PathBuf)> = snapshot.layers.iter().map(|digest| Ok((*digest, self.layer(digest)?))).collect::<Result<_>>()?;
    oci::export_snapshot(&self.cache.join("oci"), snapshot.digest, &layers, tag)
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
  pub fn commit(
    &self,
    image: &str,
    parent: Option<&Digest>,
    base: Option<&Digest>,
    upper: impl Read,
    commands: Vec<Command>,
  ) -> Result<Digest> {
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
        self.store_with(&layers, |out| Ok(merged.write(out)?))?
      }
    };
    history.image = image.to_string();
    history.commits.push(Commit {
      layer,
      parent: parent.copied(),
      base: base.copied(),
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

  /// The commit record `image` has `steps` commits before its commit `from`:
  /// `None` when that is before its first commit. The history keeps every
  /// commit, so a rolled-back one can still be booted.
  pub fn rollback(&self, image: &str, from: &Digest, steps: usize) -> Result<Option<Commit>> {
    let history = self.read_history(image)?;
    let find = |layer: &Digest| history.commits.iter().rev().find(|commit| commit.layer == *layer);
    let mut at = find(from).ok_or_else(|| Error::UnknownCommit {
      image: image.to_string(),
      digest: *from,
    })?;
    let mut chain = vec![at];
    while let Some(parent) = at.parent.as_ref().and_then(find) {
      chain.push(parent);
      at = parent;
    }
    if steps > chain.len() {
      return Err(Error::TooFar {
        image: image.to_string(),
        steps,
        available: chain.len(),
      });
    }
    Ok(chain.get(steps).copied().cloned())
  }

  /// Removes the built layers, snapshot records and disks that neither
  /// `keep` nor a commit history names. `keep` holds what the caller's
  /// live runs and lockfiles name: snapshots (with every layer in them) or
  /// layers. A commit layer that no history names (a commit cut short) is
  /// removed too.
  // NOTICE: files that are not `sha256-<hex>.<ext>` are left alone: they are
  // a write in progress (`tempfile`'s `.tmp` names) or not the store's.
  pub fn collect(&self, keep: &[Digest]) -> Result<Collected> {
    self.collect_with(keep, false)
  }

  /// Like [`Self::collect`], but when `dry_run` is true counts what would be
  /// removed without deleting any files.
  pub fn collect_with(&self, keep: &[Digest], dry_run: bool) -> Result<Collected> {
    let mut kept: HashSet<Digest> = keep.iter().copied().collect();
    for digest in keep {
      match self.resolve(digest) {
        Ok(snapshot) => {
          if snapshot.layers.len() > MAX_UNFLATTENED_LAYERS {
            let prefix_len = snapshot.layers.len() - (MAX_UNFLATTENED_LAYERS - 1);
            kept.insert(stack_digest(&snapshot.layers[..prefix_len]));
          }
          kept.extend(snapshot.layers);
        }
        Err(Error::Missing(_)) => {}
        Err(e) => return Err(e),
      }
    }
    let committed = self.committed()?;
    let mut collected = Collected::default();
    let measure_or_remove = |path: &Path| -> Result<u64> {
      if dry_run {
        match std::fs::metadata(path) {
          Ok(meta) => Ok(meta.len()),
          Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
          Err(e) => Err(io(path.display())(e)),
        }
      } else {
        remove_file(path)
      }
    };
    for (dir, is_layer) in [
      (self.cache.join("layers"), true),
      (self.data.join("layers"), true),
      (self.cache.join("disks"), false),
    ] {
      for (path, digest) in stored(&dir)? {
        if !kept.contains(&digest) && !committed.contains(&digest) {
          collected.bytes += measure_or_remove(&path)?;
          collected.layers += usize::from(is_layer);
        }
      }
    }
    for (path, digest) in stored(&self.cache.join("snapshots"))?.into_iter().chain(stored(&self.cache.join("oci"))?) {
      if !kept.contains(&digest) {
        collected.bytes += measure_or_remove(&path)?;
      }
    }
    Ok(collected)
  }

  /// The archive of a stored layer: a built layer in the cache, or a commit
  /// layer in the data directory.
  fn layer(&self, digest: &Digest) -> Result<PathBuf> {
    let name = format!("{}.tar", stem(digest));
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
    self.store_with(dir, |out| Ok(archive::canonical(layer, out)?))
  }

  /// Stores a layer as [`Store::put`] does, only if its digest is
  /// `expected`: another layer is refused ([`Error::Unexpected`]) and not
  /// kept.
  fn put_expecting(&self, layer: impl Read, expected: &Digest) -> Result<Digest> {
    self.store_checked(&self.cache.join("layers"), Some(expected), |out| Ok(archive::canonical(layer, out)?))
  }

  /// Stores the canonical form of `layer`, an uncompressed layer archive of
  /// an image another tool built whose DiffID is `diff_id`, and returns its
  /// digest. The canonical form (relative member names, no root entry, fresh
  /// headers) usually differs from what the other tool wrote, so the layer is
  /// named by its own digest and `diff_id` is recorded as its alias
  /// ([`Store::imported`]), by which the next import of it finds it.
  fn put_imported(&self, layer: impl Read, diff_id: &Digest) -> Result<Digest> {
    let digest = self.put(layer)?;
    write_atomic(&self.alias_path(diff_id), format!("{digest}\n").as_bytes())?;
    Ok(digest)
  }

  /// The stored layer an import took as DiffID `diff_id`, if it is still
  /// stored.
  // NOTICE: `collect` does not remove aliases; one whose layer it removed
  // reads as absent here and is replaced by the next import.
  fn imported(&self, diff_id: &Digest) -> Option<Digest> {
    let text = std::fs::read_to_string(self.alias_path(diff_id)).ok()?;
    let digest = Digest::parse(text.trim()).ok()?;
    self.has_layer(&digest).then_some(digest)
  }

  fn alias_path(&self, diff_id: &Digest) -> PathBuf {
    self.cache.join("imported").join(stem(diff_id))
  }

  /// Stores in `dir` the archive `write` writes, named by its digest.
  fn store_with(&self, dir: &Path, write: impl FnOnce(&mut dyn Write) -> Result<()>) -> Result<Digest> {
    self.store_checked(dir, None, write)
  }

  /// Stores in `dir` the archive `write` writes, named by its digest, which
  /// must be `expected` when there is one.
  fn store_checked(&self, dir: &Path, expected: Option<&Digest>, write: impl FnOnce(&mut dyn Write) -> Result<()>) -> Result<Digest> {
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
    let digest = Digest::from_bytes(hasher.finalize().into());
    if let Some(expected) = expected
      && *expected != digest
    {
      return Err(Error::Unexpected {
        expected: *expected,
        actual: digest,
      });
    }
    let path = dir.join(format!("{}.tar", stem(&digest)));
    if !path.exists() {
      staged.persist(&path).map_err(|e| io(path.display())(e.error))?;
    }
    Ok(digest)
  }

  fn snapshot_path(&self, digest: &Digest) -> PathBuf {
    self.cache.join("snapshots").join(format!("{}.json", stem(digest)))
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
    // `sha256-<hex>.tar`, `.json`, `.squashfs`, `-flat.squashfs`, or a disk's
    // `sha256-<hex>-on-<stack>.squashfs`.
    let digest = path.file_name().and_then(|name| name.to_str()).and_then(|name| name.get(..7 + 64)).and_then(from_stem);
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
