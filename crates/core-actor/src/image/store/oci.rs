//! Container OCI image assembly and top-layer extraction for the layer store.
//!
//! Assembles a hybrid Docker-load (`manifest.json`) and OCI Image Layout
//! (`oci-layout`, `index.json`, `blobs/sha256/...`) archive from a base
//! container image archive and a snapshot's canonical layers, running the
//! in-place shell shim step on every materialization so shells added or
//! upgraded by a build step or commit stay shimmed.
//!
//! Reference:
//! - OCI Image Layout v1.1.1:
//!   `https://github.com/opencontainers/image-spec/blob/v1.1.1/image-layout.md`
//! - OCI Image Manifest v1.1.1:
//!   `https://github.com/opencontainers/image-spec/blob/v1.1.1/manifest.md`
//! - OCI Image Config v1.1.1:
//!   `https://github.com/opencontainers/image-spec/blob/v1.1.1/config.md`
//! - Docker `manifest.json` tarexport schema:
//!   `https://github.com/moby/moby/blob/v28.5.1/image/tarexport/tarexport.go#L42-L54`

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

use sha2::Digest as _;

use super::archive::{Entry, Index, Kind};
use super::{Digest, Error, Result, io};
use crate::contract::{PROXY, REAL};

// Source: https://github.com/opencontainers/image-spec/blob/v1.1.1/media-types.md
const MEDIA_TYPE_OCI_INDEX: &str = "application/vnd.oci.image.index.v1+json";
const MEDIA_TYPE_OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const MEDIA_TYPE_OCI_CONFIG: &str = "application/vnd.oci.image.config.v1+json";
const MEDIA_TYPE_OCI_LAYER_TAR: &str = "application/vnd.oci.image.layer.v1.tar";
const MEDIA_TYPE_OCI_LAYER_TAR_GZIP: &str = "application/vnd.oci.image.layer.v1.tar+gzip";

// Source: https://datatracker.ietf.org/doc/html/rfc1952#section-2.3.1
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// Directories searched for in-place shimmed shells, relative to root.
// Source: https://github.com/moby/moby/blob/v28.5.1/oci/defaults.go#L19
// Reference: `images/container/shim.sh` and `needy_core::image::SYSTEM_PATH`.
const SHIM_DIRS: &[&str] = &[
  "usr/local/sbin",
  "usr/local/bin",
  "usr/sbin",
  "usr/bin",
  "sbin",
  "bin",
];

/// Default programs shimmed in place (the shells called by absolute path or
/// `#!` lines).
// Reference: `images/container/shim.sh` and
// `docs/ai/references/architecture/2026-11-16-container-trampolines-design.md`.
// TODO(images): `NEEDY_SHIM_NAMES` override from image config env is deferred
// until a profile or feature requests additional in-place shims.
const SHIM_NAMES: &[&str] = &["sh", "bash", "dash", "zsh"];

/// Deterministic needy-owned container image tag for a snapshot digest.
// NOTICE: needy's own repository prefix (`needy-snapshot:<64-hex>`). The full
// 64-character hex digest avoids tag collisions and fits within the OCI
// Distribution Spec's 128-character tag limit
// (`https://github.com/opencontainers/distribution-spec/blob/v1.1.1/spec.md#pulling-manifests`).
pub fn snapshot_tag(snapshot: &Digest) -> String {
  format!("needy-snapshot:{}", snapshot.hex())
}

/// Extracts the top (newest) layer from a container image archive (`docker save`
/// or OCI image layout) as an uncompressed tar stream, ready for [`super::Store::put`]
/// or [`super::Store::commit`].
pub fn extract_top_layer(image_tar: &Path) -> Result<Box<dyn Read + Send>> {
  let base = BaseArchive::open(image_tar)?;
  let top_path = base.layer_paths.last().ok_or_else(|| Error::Invalid(format!("{}: image archive has no layers", image_tar.display())))?;
  let span = base.span(top_path)?;
  open_layer_reader(image_tar, span)
}

/// Assembles a `docker load` / `container image load`-compatible archive for
/// `snapshot_digest` and `layers` on top of `base_image_tar`, tagged as `tag`,
/// cached under `oci_dir`.
pub(super) fn materialize(
  oci_dir: &Path,
  snapshot_digest: Digest,
  layers: &[(Digest, PathBuf)],
  base_image_tar: &Path,
  tag: &str,
) -> Result<PathBuf> {
  let base = BaseArchive::open(base_image_tar)?;
  let cache_key = Digest::of(format!("{snapshot_digest}\n{}\n{tag}\n", base.config_digest).as_bytes());
  let out_path = oci_dir.join(format!("{}-on-{}.tar", snapshot_digest.stem(), &cache_key.hex()[..16]));
  if out_path.exists() {
    return Ok(out_path);
  }

  let indexes: Vec<Index> = layers.iter().map(|(_, path)| Index::read(path)).collect::<Result<_>>()?;
  let shim_layer = synthesize_shim_layer(&base, &indexes)?;

  let mut config = base.config_json.clone();
  let diff_ids = ensure_base_diff_ids(&base, &mut config)?;
  for (digest, _) in layers {
    diff_ids.push(serde_json::Value::String(digest.to_string()));
  }
  if let Some((shim_digest, _)) = &shim_layer {
    diff_ids.push(serde_json::Value::String(shim_digest.to_string()));
  }
  let added_count = layers.len() + usize::from(shim_layer.is_some());
  if let Some(history) = config.get_mut("history").and_then(serde_json::Value::as_array_mut) {
    for _ in 0..added_count {
      history.push(serde_json::json!({ "created_by": "needy" }));
    }
  }

  let config_bytes = serde_json::to_vec(&config).map_err(|e| Error::Invalid(format!("serializing image config: {e}")))?;
  let config_digest = Digest::of(&config_bytes);

  let mut blobs: Vec<BlobSource> = Vec::with_capacity(base.layer_paths.len() + added_count);
  for layer_path in &base.layer_paths {
    let span = base.span(layer_path)?;
    let (blob_digest, is_gzip) = hash_and_probe_span(base_image_tar, span)?;
    blobs.push(BlobSource {
      digest: blob_digest,
      size: span.size,
      media_type: if is_gzip {
        MEDIA_TYPE_OCI_LAYER_TAR_GZIP
      } else {
        MEDIA_TYPE_OCI_LAYER_TAR
      },
      data: BlobData::BaseSpan {
        archive: base_image_tar.to_path_buf(),
        span,
      },
    });
  }
  for (digest, path) in layers {
    let size = std::fs::metadata(path).map_err(io(path.display()))?.len();
    blobs.push(BlobSource {
      digest: *digest,
      size,
      media_type: MEDIA_TYPE_OCI_LAYER_TAR,
      data: BlobData::File(path.clone()),
    });
  }
  if let Some((shim_digest, shim_bytes)) = shim_layer {
    let size = shim_bytes.len() as u64;
    blobs.push(BlobSource {
      digest: shim_digest,
      size,
      media_type: MEDIA_TYPE_OCI_LAYER_TAR,
      data: BlobData::Bytes(shim_bytes),
    });
  }

  let manifest = serde_json::json!({
    "schemaVersion": 2,
    "mediaType": MEDIA_TYPE_OCI_MANIFEST,
    "config": {
      "mediaType": MEDIA_TYPE_OCI_CONFIG,
      "digest": config_digest.to_string(),
      "size": config_bytes.len(),
    },
    "layers": blobs.iter().map(|blob| serde_json::json!({
      "mediaType": blob.media_type,
      "digest": blob.digest.to_string(),
      "size": blob.size,
    })).collect::<Vec<_>>(),
  });
  let manifest_bytes = serde_json::to_vec(&manifest).expect("an OCI manifest serializes");
  let manifest_digest = Digest::of(&manifest_bytes);

  let oci_layout_bytes = br#"{"imageLayoutVersion":"1.0.0"}"#;
  let index_json = serde_json::json!({
    "schemaVersion": 2,
    "mediaType": MEDIA_TYPE_OCI_INDEX,
    "manifests": [
      {
        "mediaType": MEDIA_TYPE_OCI_MANIFEST,
        "digest": manifest_digest.to_string(),
        "size": manifest_bytes.len(),
        "annotations": {
          "org.opencontainers.image.ref.name": tag,
          "io.containerd.image.name": tag,
        },
      }
    ],
  });
  let index_bytes = serde_json::to_vec(&index_json).expect("an OCI index serializes");

  let docker_manifest = serde_json::json!([
    {
      "Config": blob_path(&config_digest),
      "RepoTags": [tag],
      "Layers": blobs.iter().map(|blob| blob_path(&blob.digest)).collect::<Vec<_>>(),
    }
  ]);
  let docker_manifest_bytes = serde_json::to_vec(&docker_manifest).expect("a Docker manifest serializes");

  std::fs::create_dir_all(oci_dir).map_err(io(oci_dir.display()))?;
  let staged = tempfile::NamedTempFile::new_in(oci_dir).map_err(io(oci_dir.display()))?;
  {
    let mut builder = tar::Builder::new(BufWriter::new(staged.as_file().try_clone().map_err(io(staged.path().display()))?));
    append_bytes(&mut builder, "oci-layout", oci_layout_bytes)?;
    append_bytes(&mut builder, "index.json", &index_bytes)?;
    append_bytes(&mut builder, "manifest.json", &docker_manifest_bytes)?;
    append_bytes(&mut builder, &blob_path(&config_digest), &config_bytes)?;
    append_bytes(&mut builder, &blob_path(&manifest_digest), &manifest_bytes)?;

    let mut written_blobs = HashSet::new();
    for blob in &blobs {
      if !written_blobs.insert(blob.digest) {
        continue;
      }
      let path = blob_path(&blob.digest);
      match &blob.data {
        BlobData::BaseSpan { archive, span } => {
          let mut file = File::open(archive).map_err(io(archive.display()))?;
          file.seek(SeekFrom::Start(span.offset)).map_err(io(archive.display()))?;
          let mut header = regular_header(0o644, span.size);
          builder.append_data(&mut header, &path, file.take(span.size)).map_err(io("writing OCI archive"))?;
        }
        BlobData::File(layer_path) => {
          let file = File::open(layer_path).map_err(io(layer_path.display()))?;
          let mut header = regular_header(0o644, blob.size);
          builder.append_data(&mut header, &path, file).map_err(io("writing OCI archive"))?;
        }
        BlobData::Bytes(bytes) => {
          append_bytes(&mut builder, &path, bytes)?;
        }
      }
    }
    let mut writer = builder.into_inner().map_err(io("finishing OCI archive"))?;
    writer.flush().map_err(io(staged.path().display()))?;
  }

  if !out_path.exists() {
    staged.persist(&out_path).map_err(|e| io(out_path.display())(e.error))?;
  }
  Ok(out_path)
}

#[derive(Clone, Copy)]
struct TarSpan {
  offset: u64,
  size: u64,
}

struct BlobSource {
  digest: Digest,
  size: u64,
  media_type: &'static str,
  data: BlobData,
}

enum BlobData {
  BaseSpan { archive: PathBuf, span: TarSpan },
  File(PathBuf),
  Bytes(Vec<u8>),
}

/// Indexed metadata of a base container image tar (Docker save or OCI layout).
struct BaseArchive {
  path: PathBuf,
  entries: HashMap<String, TarSpan>,
  config_json: serde_json::Value,
  config_digest: Digest,
  layer_paths: Vec<String>,
}

#[derive(serde::Deserialize)]
struct DockerManifestEntry {
  #[serde(rename = "Config")]
  config: String,
  #[serde(rename = "Layers")]
  layers: Vec<String>,
}

#[derive(serde::Deserialize)]
struct OciIndex {
  manifests: Vec<OciDescriptor>,
}

#[derive(serde::Deserialize)]
struct OciManifest {
  config: OciDescriptor,
  layers: Vec<OciDescriptor>,
}

#[derive(serde::Deserialize)]
struct OciDescriptor {
  digest: String,
}

impl BaseArchive {
  fn open(path: &Path) -> Result<Self> {
    let file = File::open(path).map_err(io(path.display()))?;
    let mut archive = tar::Archive::new(BufReader::new(file));
    let mut entries = HashMap::new();
    let failed = |e: std::io::Error| io(path.display())(e);
    for entry in archive.entries_with_seek().map_err(failed)? {
      let entry = entry.map_err(failed)?;
      if !matches!(entry.header().entry_type(), tar::EntryType::Regular | tar::EntryType::Continuous) {
        continue;
      }
      let raw = entry.path().map_err(failed)?;
      if let Some(norm) = normalize_tar_path(&raw) {
        entries.insert(
          norm,
          TarSpan {
            offset: entry.raw_file_position(),
            size: entry.size(),
          },
        );
      }
    }

    let (config_path, layer_paths) = if let Some(&span) = entries.get("manifest.json") {
      let bytes = read_span(path, span)?;
      let manifests: Vec<DockerManifestEntry> =
        serde_json::from_slice(&bytes).map_err(|e| Error::Invalid(format!("{}: invalid manifest.json: {e}", path.display())))?;
      let first = manifests.into_iter().next().ok_or_else(|| Error::Invalid(format!("{}: manifest.json has no entries", path.display())))?;
      let config = normalize_str_path(&first.config)
        .ok_or_else(|| Error::Invalid(format!("{}: empty Config path in manifest.json", path.display())))?;
      let layers = first
        .layers
        .into_iter()
        .map(|layer| {
          normalize_str_path(&layer).ok_or_else(|| Error::Invalid(format!("{}: empty layer path in manifest.json", path.display())))
        })
        .collect::<Result<Vec<_>>>()?;
      (config, layers)
    } else if let Some(&span) = entries.get("index.json") {
      resolve_oci_layout(path, &entries, span)?
    } else {
      return Err(Error::Invalid(format!("{}: image archive has neither manifest.json nor index.json", path.display())));
    };

    let config_span =
      entries.get(&config_path).copied().ok_or_else(|| Error::Invalid(format!("{}: missing image config {config_path}", path.display())))?;
    let config_bytes = read_span(path, config_span)?;
    let config_digest = Digest::of(&config_bytes);
    let config_json: serde_json::Value = serde_json::from_slice(&config_bytes)
      .map_err(|e| Error::Invalid(format!("{}: invalid image config {config_path}: {e}", path.display())))?;
    if !config_json.is_object() {
      return Err(Error::Invalid(format!("{}: image config {config_path} is not a JSON object", path.display())));
    }

    Ok(Self {
      path: path.to_path_buf(),
      entries,
      config_json,
      config_digest,
      layer_paths,
    })
  }

  fn span(&self, entry_path: &str) -> Result<TarSpan> {
    self.entries.get(entry_path).copied().ok_or_else(|| Error::Invalid(format!("{}: missing entry {entry_path}", self.path.display())))
  }
}

fn resolve_oci_layout(path: &Path, entries: &HashMap<String, TarSpan>, index_span: TarSpan) -> Result<(String, Vec<String>)> {
  let index_bytes = read_span(path, index_span)?;
  let index: OciIndex =
    serde_json::from_slice(&index_bytes).map_err(|e| Error::Invalid(format!("{}: invalid index.json: {e}", path.display())))?;
  let first =
    index.manifests.into_iter().next().ok_or_else(|| Error::Invalid(format!("{}: index.json has no manifests", path.display())))?;
  let mut manifest_path = digest_to_blob_path(path, &first.digest)?;

  // An OCI image index may point to a nested index (multi-arch); follow one level.
  let manifest_span = entries
    .get(&manifest_path)
    .copied()
    .ok_or_else(|| Error::Invalid(format!("{}: missing manifest blob {manifest_path}", path.display())))?;
  let mut manifest_bytes = read_span(path, manifest_span)?;
  if let Ok(nested) = serde_json::from_slice::<OciIndex>(&manifest_bytes)
    && let Some(desc) = nested.manifests.into_iter().next()
  {
    manifest_path = digest_to_blob_path(path, &desc.digest)?;
    let nested_span = entries
      .get(&manifest_path)
      .copied()
      .ok_or_else(|| Error::Invalid(format!("{}: missing nested manifest blob {manifest_path}", path.display())))?;
    manifest_bytes = read_span(path, nested_span)?;
  }

  let manifest: OciManifest = serde_json::from_slice(&manifest_bytes)
    .map_err(|e| Error::Invalid(format!("{}: invalid OCI manifest {manifest_path}: {e}", path.display())))?;
  let config_path = digest_to_blob_path(path, &manifest.config.digest)?;
  let layer_paths = manifest.layers.into_iter().map(|layer| digest_to_blob_path(path, &layer.digest)).collect::<Result<Vec<_>>>()?;
  Ok((config_path, layer_paths))
}

fn digest_to_blob_path(archive: &Path, digest: &str) -> Result<String> {
  let parsed: Digest =
    digest.parse().map_err(|_| Error::Invalid(format!("{}: unsupported or invalid digest {digest:?}", archive.display())))?;
  Ok(blob_path(&parsed))
}

fn blob_path(digest: &Digest) -> String {
  format!("blobs/sha256/{}", digest.hex())
}

fn ensure_base_diff_ids<'a>(base: &BaseArchive, config: &'a mut serde_json::Value) -> Result<&'a mut Vec<serde_json::Value>> {
  let obj = config.as_object_mut().ok_or_else(|| Error::Invalid("image config is not a JSON object".into()))?;
  let rootfs = obj.entry("rootfs").or_insert_with(|| serde_json::json!({ "type": "layers", "diff_ids": [] }));
  let rootfs_obj = rootfs.as_object_mut().ok_or_else(|| Error::Invalid("image config rootfs is not a JSON object".into()))?;
  rootfs_obj.entry("type").or_insert_with(|| serde_json::Value::String("layers".into()));
  let diff_ids = rootfs_obj.entry("diff_ids").or_insert_with(|| serde_json::Value::Array(Vec::new()));
  let arr = diff_ids.as_array_mut().ok_or_else(|| Error::Invalid("image config rootfs.diff_ids is not an array".into()))?;

  if arr.len() != base.layer_paths.len() {
    arr.clear();
    for layer_path in &base.layer_paths {
      let span = base.span(layer_path)?;
      let mut reader = open_layer_reader(&base.path, span)?;
      let mut hasher = sha2::Sha256::new();
      std::io::copy(&mut reader, &mut hasher).map_err(io(base.path.display()))?;
      let digest = Digest(hasher.finalize().into());
      arr.push(serde_json::Value::String(digest.to_string()));
    }
  }
  Ok(arr)
}

enum ShimTarget {
  File { entry: Entry, bytes: Vec<u8> },
  Symlink(PathBuf),
}

/// Synthesizes a top shim layer if any layer in `indexes` added or replaced an
/// in-place shell binary (`sh`, `bash`, `dash`, `zsh` under [`SHIM_DIRS`]).
/// Returns `None` when no shell needs shimming.
fn synthesize_shim_layer(base: &BaseArchive, indexes: &[Index]) -> Result<Option<(Digest, Vec<u8>)>> {
  let mut touched: BTreeMap<PathBuf, Entry> = BTreeMap::new();
  for index in indexes {
    for removed in &index.whiteouts {
      touched.retain(|path, _| path != removed && !path.starts_with(removed));
    }
    for dir in &index.opaque {
      touched.retain(|path, _| path == dir || !path.starts_with(dir));
    }
    for (at, entry) in &index.entries {
      if !entry.is_dir() {
        touched.retain(|path, _| path == at || !path.starts_with(at));
      }
      if is_shim_candidate(at) {
        let resolved = index.resolve(at)?;
        touched.insert(at.clone(), resolved);
      }
    }
  }

  if touched.is_empty() {
    return Ok(None);
  }

  let mut proxy_bytes: Option<Vec<u8>> = None;
  let mut to_shim: Vec<(PathBuf, ShimTarget)> = Vec::new();
  for (path, entry) in touched {
    match &entry.kind {
      Kind::File(data) if entry.mode & 0o111 != 0 => {
        let mut bytes = Vec::with_capacity(data.size as usize);
        data.reader().read_to_end(&mut bytes).map_err(io("reading shell binary from layer"))?;
        let proxy = match &proxy_bytes {
          Some(proxy) => proxy.as_slice(),
          None => proxy_bytes.insert(find_proxy_bytes(base, indexes)?).as_slice(),
        };
        if bytes.as_slice() == proxy {
          continue;
        }
        to_shim.push((path, ShimTarget::File { entry, bytes }));
      }
      Kind::Symlink(target) => {
        if symlink_stays_in_shim_dirs(&path, target) {
          continue;
        }
        // Ensure the proxy exists before deciding to shim.
        if proxy_bytes.is_none() {
          proxy_bytes = Some(find_proxy_bytes(base, indexes)?);
        }
        to_shim.push((path, ShimTarget::Symlink(target.clone())));
      }
      _ => {}
    }
  }

  if to_shim.is_empty() {
    return Ok(None);
  }

  let proxy = proxy_bytes.expect("loaded above");
  let real_root = Path::new(REAL.trim_start_matches('/'));
  let mut builder = tar::Builder::new(Vec::new());
  let mut created_dirs: BTreeSet<PathBuf> = BTreeSet::new();

  for (shell_path, target) in to_shim {
    let real_path = real_root.join(&shell_path);
    ensure_parent_dirs(&mut builder, &mut created_dirs, &real_path)?;
    match target {
      ShimTarget::File { entry, bytes } => {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(entry.mode & 0o7777);
        header.set_uid(entry.uid);
        header.set_gid(entry.gid);
        header.set_mtime(entry.mtime);
        header.set_size(bytes.len() as u64);
        builder.append_data(&mut header, &real_path, bytes.as_slice()).map_err(io("writing shim layer"))?;
      }
      ShimTarget::Symlink(link_target) => {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_mode(0o777);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_size(0);
        builder.append_link(&mut header, &real_path, &link_target).map_err(io("writing shim layer"))?;
      }
    }

    ensure_parent_dirs(&mut builder, &mut created_dirs, &shell_path)?;
    let mut proxy_header = regular_header(0o755, proxy.len() as u64);
    builder.append_data(&mut proxy_header, &shell_path, proxy.as_slice()).map_err(io("writing shim layer"))?;
  }

  let tar_bytes = builder.into_inner().map_err(io("finishing shim layer"))?;
  let digest = Digest::of(&tar_bytes);
  Ok(Some((digest, tar_bytes)))
}

fn ensure_parent_dirs<W: Write>(builder: &mut tar::Builder<W>, created: &mut BTreeSet<PathBuf>, path: &Path) -> Result<()> {
  let mut parents: Vec<PathBuf> = path.ancestors().skip(1).filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf).collect();
  parents.reverse();
  for dir in parents {
    if created.insert(dir.clone()) {
      let mut header = tar::Header::new_gnu();
      header.set_entry_type(tar::EntryType::Directory);
      header.set_mode(0o755);
      header.set_uid(0);
      header.set_gid(0);
      header.set_mtime(0);
      header.set_size(0);
      builder.append_data(&mut header, &dir, std::io::empty()).map_err(io("writing shim layer"))?;
    }
  }
  Ok(())
}

/// Finds `/.needy/proxy` bytes, checking snapshot layers top-down first and
/// then base image layers top-down.
fn find_proxy_bytes(base: &BaseArchive, indexes: &[Index]) -> Result<Vec<u8>> {
  let proxy_rel = Path::new(PROXY.trim_start_matches('/'));
  for index in indexes.iter().rev() {
    if index.entries.contains_key(proxy_rel)
      && let Kind::File(data) = index.resolve(proxy_rel)?.kind
    {
      let mut bytes = Vec::with_capacity(data.size as usize);
      data.reader().read_to_end(&mut bytes).map_err(io("reading /.needy/proxy from layer"))?;
      return Ok(bytes);
    }
  }

  let proxy_str = proxy_rel.to_string_lossy();
  for layer_path in base.layer_paths.iter().rev() {
    let span = base.span(layer_path)?;
    let reader = open_layer_reader(&base.path, span)?;
    let mut layer_tar = tar::Archive::new(reader);
    let failed = |e: std::io::Error| Error::Invalid(format!("{}: reading base layer {layer_path}: {e}", base.path.display()));
    for entry in layer_tar.entries().map_err(failed)? {
      let mut entry = entry.map_err(failed)?;
      if !matches!(entry.header().entry_type(), tar::EntryType::Regular | tar::EntryType::Continuous) {
        continue;
      }
      let raw = entry.path().map_err(failed)?;
      if normalize_tar_path(&raw).as_deref() == Some(proxy_str.as_ref()) {
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).map_err(failed)?;
        return Ok(bytes);
      }
    }
  }

  Err(Error::Invalid(format!("base image {} has no {PROXY}; cannot shim shells", base.path.display())))
}

fn is_shim_candidate(path: &Path) -> bool {
  let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
    return false;
  };
  if !SHIM_NAMES.contains(&name) {
    return false;
  }
  let Some(parent) = path.parent().and_then(|p| p.to_str()) else {
    return false;
  };
  SHIM_DIRS.contains(&parent)
}

/// Whether a symlink at `shell_path` pointing to `target` resolves inside one
/// of [`SHIM_DIRS`] (in which case `images/container/shim.sh` leaves the
/// symlink alone, because its target is already a shim or real program in a
/// shimmed directory).
fn symlink_stays_in_shim_dirs(shell_path: &Path, target: &Path) -> bool {
  let combined = if target.is_absolute() {
    target.to_path_buf()
  } else {
    shell_path.parent().unwrap_or(Path::new("")).join(target)
  };
  let mut parts: Vec<&std::ffi::OsStr> = Vec::new();
  for component in combined.components() {
    match component {
      Component::Normal(part) => parts.push(part),
      Component::ParentDir => {
        parts.pop();
      }
      Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
    }
  }
  let resolved: PathBuf = parts.into_iter().collect();
  resolved.parent().and_then(|p| p.to_str()).is_some_and(|dir| SHIM_DIRS.contains(&dir))
}

fn open_layer_reader(archive: &Path, span: TarSpan) -> Result<Box<dyn Read + Send>> {
  let mut file = File::open(archive).map_err(io(archive.display()))?;
  file.seek(SeekFrom::Start(span.offset)).map_err(io(archive.display()))?;
  let mut magic = [0u8; 2];
  let is_gzip = span.size >= 2 && file.read_exact(&mut magic).is_ok() && magic == GZIP_MAGIC;
  file.seek(SeekFrom::Start(span.offset)).map_err(io(archive.display()))?;
  let bounded = file.take(span.size);
  if is_gzip {
    Ok(Box::new(flate2::read::GzDecoder::new(bounded)))
  } else {
    Ok(Box::new(bounded))
  }
}

fn hash_and_probe_span(archive: &Path, span: TarSpan) -> Result<(Digest, bool)> {
  let mut file = File::open(archive).map_err(io(archive.display()))?;
  file.seek(SeekFrom::Start(span.offset)).map_err(io(archive.display()))?;
  let mut bounded = file.take(span.size);
  let mut hasher = sha2::Sha256::new();
  let mut buf = [0u8; 16 * 1024];
  let mut first = [0u8; 2];
  let mut first_len = 0usize;
  loop {
    let n = bounded.read(&mut buf).map_err(io(archive.display()))?;
    if n == 0 {
      break;
    }
    if first_len < 2 {
      let take = (2 - first_len).min(n);
      first[first_len..first_len + take].copy_from_slice(&buf[..take]);
      first_len += take;
    }
    hasher.update(&buf[..n]);
  }
  let is_gzip = first_len == 2 && first == GZIP_MAGIC;
  Ok((Digest(hasher.finalize().into()), is_gzip))
}

fn read_span(archive: &Path, span: TarSpan) -> Result<Vec<u8>> {
  let mut file = File::open(archive).map_err(io(archive.display()))?;
  file.seek(SeekFrom::Start(span.offset)).map_err(io(archive.display()))?;
  let mut buf = Vec::with_capacity(span.size as usize);
  file.take(span.size).read_to_end(&mut buf).map_err(io(archive.display()))?;
  Ok(buf)
}

fn normalize_tar_path(path: &Path) -> Option<String> {
  let mut parts: Vec<String> = Vec::new();
  for component in path.components() {
    match component {
      Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
      Component::RootDir | Component::CurDir => {}
      Component::ParentDir | Component::Prefix(_) => return None,
    }
  }
  (!parts.is_empty()).then(|| parts.join("/"))
}

fn normalize_str_path(raw: &str) -> Option<String> {
  normalize_tar_path(Path::new(raw))
}

fn regular_header(mode: u32, size: u64) -> tar::Header {
  let mut header = tar::Header::new_gnu();
  header.set_entry_type(tar::EntryType::Regular);
  header.set_mode(mode);
  header.set_uid(0);
  header.set_gid(0);
  header.set_mtime(0);
  header.set_size(size);
  header
}

fn append_bytes<W: Write>(builder: &mut tar::Builder<W>, path: &str, bytes: &[u8]) -> Result<()> {
  let mut header = regular_header(0o644, bytes.len() as u64);
  builder.append_data(&mut header, path, bytes).map_err(io("writing OCI archive"))
}

#[cfg(test)]
#[path = "oci_test.rs"]
mod tests;
