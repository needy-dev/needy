//! Generic OCI Image Layout and Docker `tarexport` reading and hybrid archive
//! assembly.
//!
//! Assembles hybrid Docker-load (`manifest.json`) + OCI Image Layout
//! (`oci-layout`, `index.json`, `blobs/sha256/...`) tar archives using
//! `oci-spec` types, with caller-supplied post-layer and config-mutation hooks.
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

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

use oci_spec::image::{
  ANNOTATION_REF_NAME, Descriptor, DescriptorBuilder, ImageIndex, ImageIndexBuilder, ImageManifest, ImageManifestBuilder, MediaType,
  OciLayoutBuilder, SCHEMA_VERSION,
};
use sha2::Digest as _;

use crate::{Digest, Error, Result, io};

// Source: https://datatracker.ietf.org/doc/html/rfc1952#section-2.3.1
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// OCI Image Layout blob path for `digest`: `blobs/sha256/<hex>`.
pub fn blob_path(digest: &Digest) -> String {
  format!("blobs/sha256/{}", digest.hex())
}

/// Extracts the top (newest) layer from a container image archive (`docker save`
/// or OCI image layout) as an uncompressed tar stream.
pub fn extract_top_layer(image_tar: &Path) -> Result<Box<dyn Read + Send>> {
  let base = BaseArchive::open(image_tar)?;
  let top_path = base.layer_paths.last().ok_or_else(|| Error::Invalid(format!("{}: image archive has no layers", image_tar.display())))?;
  let span = base.span(top_path)?;
  open_layer_reader(image_tar, span)
}

/// Assembles a hybrid `docker load` / OCI Image Layout archive by stacking
/// `layers` (each `(diff_id, tar_path)`) on top of `base`, running an optional
/// `post_layer` hook (which may return one synthesized uncompressed tar layer)
/// and a `mutate_config` hook on the resulting image config JSON, and writing
/// atomically to `out_path`.
pub fn assemble_on_base<F, C>(
  base: &BaseArchive,
  layers: &[(Digest, PathBuf)],
  tag: &str,
  out_path: &Path,
  history_created_by: &str,
  post_layer: F,
  mutate_config: C,
) -> Result<()>
where
  F: FnOnce(&BaseArchive) -> Result<Option<(Digest, Vec<u8>)>>,
  C: FnOnce(&mut serde_json::Value),
{
  let extra_layer = post_layer(base)?;

  let mut config = base.config_json.clone();
  mutate_config(&mut config);
  let diff_ids = ensure_base_diff_ids(base, &mut config)?;
  for (digest, _) in layers {
    diff_ids.push(serde_json::Value::String(digest.to_string()));
  }
  if let Some((extra_digest, _)) = &extra_layer {
    diff_ids.push(serde_json::Value::String(extra_digest.to_string()));
  }
  let added_count = layers.len() + usize::from(extra_layer.is_some());
  if let Some(history) = config.get_mut("history").and_then(serde_json::Value::as_array_mut) {
    for _ in 0..added_count {
      history.push(serde_json::json!({ "created_by": history_created_by }));
    }
  }

  let config_bytes = serde_json::to_vec(&config).map_err(|e| Error::Invalid(format!("serializing image config: {e}")))?;
  let config_digest = Digest::of(&config_bytes);

  let mut blobs: Vec<BlobSource> = Vec::with_capacity(base.layer_paths.len() + added_count);
  for layer_path in &base.layer_paths {
    let span = base.span(layer_path)?;
    let (blob_digest, is_gzip) = hash_and_probe_span(&base.path, span)?;
    blobs.push(BlobSource {
      digest: blob_digest,
      size: span.size,
      media_type: if is_gzip {
        MediaType::ImageLayerGzip
      } else {
        MediaType::ImageLayer
      },
      data: BlobData::BaseSpan {
        archive: base.path.clone(),
        span,
      },
    });
  }
  for (digest, path) in layers {
    let size = std::fs::metadata(path).map_err(io(path.display()))?.len();
    blobs.push(BlobSource {
      digest: *digest,
      size,
      media_type: MediaType::ImageLayer,
      data: BlobData::File(path.clone()),
    });
  }
  if let Some((extra_digest, extra_bytes)) = extra_layer {
    let size = extra_bytes.len() as u64;
    blobs.push(BlobSource {
      digest: extra_digest,
      size,
      media_type: MediaType::ImageLayer,
      data: BlobData::Bytes(extra_bytes),
    });
  }

  write_hybrid_archive(out_path, &config_bytes, config_digest, &blobs, tag)
}

/// Assembles a standalone hybrid `docker load` / OCI Image Layout archive
/// containing `layers` (without a base container archive), tagged as `tag`,
/// and writes it atomically to `out_path`.
pub fn export_layers<C>(
  layers: &[(Digest, PathBuf)],
  arch: &str,
  os: &str,
  tag: &str,
  out_path: &Path,
  history_created_by: &str,
  mutate_config: C,
) -> Result<()>
where
  C: FnOnce(&mut serde_json::Value),
{
  let diff_ids: Vec<serde_json::Value> = layers.iter().map(|(digest, _)| serde_json::Value::String(digest.to_string())).collect();
  let history: Vec<serde_json::Value> = layers.iter().map(|_| serde_json::json!({ "created_by": history_created_by })).collect();
  let mut config = serde_json::json!({
    "architecture": arch,
    "os": os,
    "config": {},
    "rootfs": {
      "type": "layers",
      "diff_ids": diff_ids,
    },
    "history": history,
  });
  mutate_config(&mut config);

  let config_bytes = serde_json::to_vec(&config).map_err(|e| Error::Invalid(format!("serializing image config: {e}")))?;
  let config_digest = Digest::of(&config_bytes);

  let mut blobs: Vec<BlobSource> = Vec::with_capacity(layers.len());
  for (digest, path) in layers {
    let size = std::fs::metadata(path).map_err(io(path.display()))?.len();
    blobs.push(BlobSource {
      digest: *digest,
      size,
      media_type: MediaType::ImageLayer,
      data: BlobData::File(path.clone()),
    });
  }

  write_hybrid_archive(out_path, &config_bytes, config_digest, &blobs, tag)
}

/// Normalizes an image reference for `io.containerd.image.name` in OCI
/// `index.json` using Docker's canonical domain rules (`alpine:latest` ->
/// `docker.io/library/alpine:latest`, `org/repo:tag` ->
/// `docker.io/org/repo:tag`, explicit registries unchanged).
// Source: https://github.com/distribution/reference/blob/v0.6.0/normalize.go#L112-L133
// Reference: Docker Engine containerd image store (`driver-type: io.containerd.snapshotter.v1`);
// `docker load` stores the verbatim `io.containerd.image.name` key while
// `docker inspect` / `docker create` normalize short names to `docker.io/library/...`.
fn containerd_image_name(reference: &str) -> String {
  let normalized = match reference.split_once('/') {
    None => format!("docker.io/library/{reference}"),
    Some((first, _)) if first.contains('.') || first.contains(':') || first == "localhost" => reference.to_string(),
    Some(_) => format!("docker.io/{reference}"),
  };
  let tail = normalized.rsplit_once('/').map_or(normalized.as_str(), |(_, tail)| tail);
  if tail.contains('@') || tail.contains(':') {
    normalized
  } else {
    format!("{normalized}:latest")
  }
}

/// Writes the hybrid OCI Image Layout (`oci-layout`, `index.json`,
/// `blobs/sha256/...`) and Docker `tarexport` (`manifest.json`) tar archive
/// atomically to `out_path`.
fn write_hybrid_archive(out_path: &Path, config_bytes: &[u8], config_digest: Digest, blobs: &[BlobSource], tag: &str) -> Result<()> {
  let config_descriptor = Descriptor::new(MediaType::ImageConfig, config_bytes.len() as u64, config_digest);
  let layer_descriptors: Vec<Descriptor> =
    blobs.iter().map(|blob| Descriptor::new(blob.media_type.clone(), blob.size, blob.digest)).collect();
  let manifest = ImageManifestBuilder::default()
    .schema_version(SCHEMA_VERSION)
    .media_type(MediaType::ImageManifest)
    .config(config_descriptor)
    .layers(layer_descriptors)
    .build()
    .map_err(|e| Error::Invalid(format!("building OCI manifest: {e}")))?;
  let manifest_bytes = serde_json::to_vec(&manifest).expect("an OCI manifest serializes");
  let manifest_digest = Digest::of(&manifest_bytes);

  let oci_layout =
    OciLayoutBuilder::default().image_layout_version("1.0.0").build().map_err(|e| Error::Invalid(format!("building oci-layout: {e}")))?;
  let oci_layout_bytes = serde_json::to_vec(&oci_layout).expect("an oci-layout serializes");

  let annotations = HashMap::from([
    (ANNOTATION_REF_NAME.to_string(), tag.to_string()),
    ("io.containerd.image.name".to_string(), containerd_image_name(tag)),
  ]);
  let manifest_descriptor = DescriptorBuilder::default()
    .media_type(MediaType::ImageManifest)
    .digest(manifest_digest)
    .size(manifest_bytes.len() as u64)
    .annotations(annotations)
    .build()
    .map_err(|e| Error::Invalid(format!("building OCI index descriptor: {e}")))?;
  let index = ImageIndexBuilder::default()
    .schema_version(SCHEMA_VERSION)
    .media_type(MediaType::ImageIndex)
    .manifests(vec![manifest_descriptor])
    .build()
    .map_err(|e| Error::Invalid(format!("building OCI index: {e}")))?;
  let index_bytes = serde_json::to_vec(&index).expect("an OCI index serializes");

  let docker_manifest = serde_json::json!([
    {
      "Config": blob_path(&config_digest),
      "RepoTags": [tag],
      "Layers": blobs.iter().map(|blob| blob_path(&blob.digest)).collect::<Vec<_>>(),
    }
  ]);
  let docker_manifest_bytes = serde_json::to_vec(&docker_manifest).expect("a Docker manifest serializes");

  let out_dir = out_path.parent().expect("an OCI archive path has a parent directory");
  std::fs::create_dir_all(out_dir).map_err(io(out_dir.display()))?;
  let staged = tempfile::NamedTempFile::new_in(out_dir).map_err(io(out_dir.display()))?;
  {
    let mut builder = tar::Builder::new(BufWriter::new(staged.as_file().try_clone().map_err(io(staged.path().display()))?));
    append_bytes(&mut builder, "oci-layout", &oci_layout_bytes)?;
    append_bytes(&mut builder, "index.json", &index_bytes)?;
    append_bytes(&mut builder, "manifest.json", &docker_manifest_bytes)?;
    append_bytes(&mut builder, &blob_path(&config_digest), config_bytes)?;
    append_bytes(&mut builder, &blob_path(&manifest_digest), &manifest_bytes)?;

    let mut written_blobs = HashSet::new();
    for blob in blobs {
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
    staged.persist(out_path).map_err(|e| io(out_path.display())(e.error))?;
  }
  Ok(())
}

#[derive(Clone, Copy)]
struct TarSpan {
  offset: u64,
  size: u64,
}

struct BlobSource {
  digest: Digest,
  size: u64,
  media_type: MediaType,
  data: BlobData,
}

enum BlobData {
  BaseSpan { archive: PathBuf, span: TarSpan },
  File(PathBuf),
  Bytes(Vec<u8>),
}

/// Indexed metadata of a base container image tar (Docker save or OCI layout).
pub struct BaseArchive {
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

impl BaseArchive {
  /// Indexes a Docker-save or OCI Image Layout tar archive at `path`.
  pub fn open(path: &Path) -> Result<Self> {
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

  /// Path to the base archive on disk.
  pub fn path(&self) -> &Path {
    &self.path
  }

  /// SHA-256 digest of the base image config JSON blob.
  pub fn config_digest(&self) -> Digest {
    self.config_digest
  }

  /// Parsed base image config JSON.
  pub fn config_json(&self) -> &serde_json::Value {
    &self.config_json
  }

  /// Tar paths of the base image's layers, lowest first.
  pub fn layer_paths(&self) -> &[String] {
    &self.layer_paths
  }

  /// Searches the base image's layers top-down for a regular file at
  /// `rel_path` (normalized POSIX relative path, e.g. `.needy/proxy`) and
  /// returns its bytes if found.
  pub fn find_regular_file_top_down(&self, rel_path: &str) -> Result<Option<Vec<u8>>> {
    let wanted = normalize_str_path(rel_path).ok_or_else(|| Error::Invalid(format!("invalid relative path {rel_path:?}")))?;
    for layer_path in self.layer_paths.iter().rev() {
      let span = self.span(layer_path)?;
      let reader = open_layer_reader(&self.path, span)?;
      let mut layer_tar = tar::Archive::new(reader);
      let failed = |e: std::io::Error| Error::Invalid(format!("{}: reading base layer {layer_path}: {e}", self.path.display()));
      for entry in layer_tar.entries().map_err(failed)? {
        let mut entry = entry.map_err(failed)?;
        if !matches!(entry.header().entry_type(), tar::EntryType::Regular | tar::EntryType::Continuous) {
          continue;
        }
        let raw = entry.path().map_err(failed)?;
        if normalize_tar_path(&raw).as_deref() == Some(wanted.as_str()) {
          let mut bytes = Vec::new();
          entry.read_to_end(&mut bytes).map_err(failed)?;
          return Ok(Some(bytes));
        }
      }
    }
    Ok(None)
  }

  fn span(&self, entry_path: &str) -> Result<TarSpan> {
    self.entries.get(entry_path).copied().ok_or_else(|| Error::Invalid(format!("{}: missing entry {entry_path}", self.path.display())))
  }
}

fn resolve_oci_layout(path: &Path, entries: &HashMap<String, TarSpan>, index_span: TarSpan) -> Result<(String, Vec<String>)> {
  let index_bytes = read_span(path, index_span)?;
  let index: ImageIndex =
    serde_json::from_slice(&index_bytes).map_err(|e| Error::Invalid(format!("{}: invalid index.json: {e}", path.display())))?;
  let first = index.manifests().first().ok_or_else(|| Error::Invalid(format!("{}: index.json has no manifests", path.display())))?;
  let mut manifest_path = digest_to_blob_path(path, first.digest().as_ref())?;

  // An OCI image index may point to a nested index (multi-arch); follow one level.
  let manifest_span = entries
    .get(&manifest_path)
    .copied()
    .ok_or_else(|| Error::Invalid(format!("{}: missing manifest blob {manifest_path}", path.display())))?;
  let mut manifest_bytes = read_span(path, manifest_span)?;
  if let Ok(nested) = serde_json::from_slice::<ImageIndex>(&manifest_bytes)
    && let Some(desc) = nested.manifests().first()
  {
    manifest_path = digest_to_blob_path(path, desc.digest().as_ref())?;
    let nested_span = entries
      .get(&manifest_path)
      .copied()
      .ok_or_else(|| Error::Invalid(format!("{}: missing nested manifest blob {manifest_path}", path.display())))?;
    manifest_bytes = read_span(path, nested_span)?;
  }

  let manifest: ImageManifest = serde_json::from_slice(&manifest_bytes)
    .map_err(|e| Error::Invalid(format!("{}: invalid OCI manifest {manifest_path}: {e}", path.display())))?;
  let config_path = digest_to_blob_path(path, manifest.config().digest().as_ref())?;
  let layer_paths = manifest.layers().iter().map(|layer| digest_to_blob_path(path, layer.digest().as_ref())).collect::<Result<Vec<_>>>()?;
  Ok((config_path, layer_paths))
}

fn digest_to_blob_path(archive: &Path, digest: &str) -> Result<String> {
  let parsed: Digest =
    digest.parse().map_err(|_| Error::Invalid(format!("{}: unsupported or invalid digest {digest:?}", archive.display())))?;
  Ok(blob_path(&parsed))
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
      let digest = Digest::from_bytes(hasher.finalize().into());
      arr.push(serde_json::Value::String(digest.to_string()));
    }
  }
  Ok(arr)
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
  Ok((Digest::from_bytes(hasher.finalize().into()), is_gzip))
}

fn read_span(archive: &Path, span: TarSpan) -> Result<Vec<u8>> {
  let mut file = File::open(archive).map_err(io(archive.display()))?;
  file.seek(SeekFrom::Start(span.offset)).map_err(io(archive.display()))?;
  let mut buf = Vec::with_capacity(span.size as usize);
  file.take(span.size).read_to_end(&mut buf).map_err(io(archive.display()))?;
  Ok(buf)
}

/// Normalizes a tar path into a clean slash-separated relative POSIX path,
/// returning `None` for empty/root paths or paths containing `..` or prefixes.
pub fn normalize_tar_path(path: &Path) -> Option<String> {
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
