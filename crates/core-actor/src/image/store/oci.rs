//! Needy container OCI image assembly and shell-shim layer synthesis.
//!
//! Delegates generic OCI Image Layout / Docker `tarexport` reading and hybrid
//! archive assembly to [`oci_layer::oci`], supplying needy's in-place shell
//! shim layer synthesis (`/.needy/proxy`, `/.needy/real`) and container config
//! invariants (`needy.base-abi` label, `/.needy/init` entrypoint).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use oci_layer::archive::{Entry, Index, Kind};
use oci_layer::oci::{BaseArchive, assemble_on_base, export_layers, normalize_tar_path};

use super::{Digest, Result, to_oci_digest};
use crate::contract::{PROXY, REAL};

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
  Ok(oci_layer::oci::extract_top_layer(image_tar)?)
}

/// Ensures the materialized container image config carries needy's
/// `needy.base-abi` label and `/.needy/init` entrypoint when absent on an
/// arbitrary OCI root image.
// TODO(images): injecting the static musl `/.needy/{init,proxy}` stubs layer
// into arbitrary non-needy OCI container roots (built from `images/stub/` in
// `images/Dockerfile`) is deferred until release artifacts bundle those stubs
// alongside the guest needy binary.
fn ensure_needy_container_config(config: &mut serde_json::Value) {
  let Some(root) = config.as_object_mut() else {
    return;
  };
  let cfg = root.entry("config").or_insert_with(|| serde_json::json!({}));
  let Some(cfg_obj) = cfg.as_object_mut() else {
    return;
  };
  let labels = cfg_obj.entry("Labels").or_insert_with(|| serde_json::json!({}));
  if let Some(labels_obj) = labels.as_object_mut() {
    labels_obj.entry(crate::image::base::LABEL).or_insert_with(|| serde_json::Value::String(crate::image::base::ABI.to_string()));
  }
  let missing_entrypoint = cfg_obj.get("Entrypoint").and_then(serde_json::Value::as_array).is_none_or(|arr| arr.is_empty());
  if missing_entrypoint {
    cfg_obj.insert("Entrypoint".to_string(), serde_json::json!([crate::contract::INIT]));
  }
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
  // NOTICE: `oci-v2` invalidates pre-fix cached archives whose `index.json`
  // wrote an unnormalized `io.containerd.image.name` (rejected on lookup by
  // Docker's containerd image store).
  let cache_key = Digest::of(format!("oci-v2\n{snapshot_digest}\n{}\n{tag}\n", base.config_digest()).as_bytes());
  let out_path = oci_dir.join(format!("{}-on-{}.tar", super::stem(&snapshot_digest), &cache_key.hex()[..16]));
  if out_path.exists() {
    return Ok(out_path);
  }

  let oci_layers: Vec<(oci_layer::Digest, PathBuf)> = layers.iter().map(|(d, p)| (to_oci_digest(*d), p.clone())).collect();
  assemble_on_base(
    &base,
    &oci_layers,
    tag,
    &out_path,
    "needy",
    |base| {
      let indexes: Vec<Index> = oci_layers.iter().map(|(_, path)| Index::read(path)).collect::<oci_layer::Result<_>>()?;
      synthesize_shim_layer(base, &indexes)
    },
    ensure_needy_container_config,
  )?;
  Ok(out_path)
}

/// Assembles a standalone `docker load` / `container image load`-compatible
/// OCI archive containing `layers` (without a container base tar), tagged as
/// `tag`, cached under `oci_dir`. Used when exporting a snapshot built on a VM
/// driver where no base container tar is available on the host.
pub(super) fn export_snapshot(oci_dir: &Path, snapshot_digest: Digest, layers: &[(Digest, PathBuf)], tag: &str) -> Result<PathBuf> {
  let cache_key = Digest::of(format!("oci-v2\nexport\n{snapshot_digest}\n{tag}\n").as_bytes());
  let out_path = oci_dir.join(format!("{}-export-{}.tar", super::stem(&snapshot_digest), &cache_key.hex()[..16]));
  if out_path.exists() {
    return Ok(out_path);
  }

  let arch = crate::image::container::oci_arch(std::env::consts::ARCH);
  let oci_layers: Vec<(oci_layer::Digest, PathBuf)> = layers.iter().map(|(d, p)| (to_oci_digest(*d), p.clone())).collect();
  export_layers(&oci_layers, arch, "linux", tag, &out_path, "needy", ensure_needy_container_config)?;
  Ok(out_path)
}

enum ShimTarget {
  File { entry: Entry, bytes: Vec<u8> },
  Symlink(PathBuf),
}

/// Synthesizes a top shim layer if any layer in `indexes` added or replaced an
/// in-place shell binary (`sh`, `bash`, `dash`, `zsh` under [`SHIM_DIRS`]).
/// Returns `None` when no shell needs shimming.
fn synthesize_shim_layer(base: &BaseArchive, indexes: &[Index]) -> oci_layer::Result<Option<(oci_layer::Digest, Vec<u8>)>> {
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
        data.reader().read_to_end(&mut bytes).map_err(|source| oci_layer::Error::Io {
          context: "reading shell binary from layer".into(),
          source,
        })?;
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
  let real_root: PathBuf = REAL.trim_start_matches('/').split('/').collect();
  let mut builder = tar::Builder::new(Vec::new());
  let mut created_dirs: BTreeSet<PathBuf> = BTreeSet::new();
  let shim_io = |source| oci_layer::Error::Io {
    context: "writing shim layer".into(),
    source,
  };

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
        builder.append_data(&mut header, &real_path, bytes.as_slice()).map_err(shim_io)?;
      }
      ShimTarget::Symlink(link_target) => {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_mode(0o777);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_size(0);
        builder.append_link(&mut header, &real_path, &link_target).map_err(shim_io)?;
      }
    }

    ensure_parent_dirs(&mut builder, &mut created_dirs, &shell_path)?;
    let mut proxy_header = tar::Header::new_gnu();
    proxy_header.set_entry_type(tar::EntryType::Regular);
    proxy_header.set_mode(0o755);
    proxy_header.set_uid(0);
    proxy_header.set_gid(0);
    proxy_header.set_mtime(0);
    proxy_header.set_size(proxy.len() as u64);
    builder.append_data(&mut proxy_header, &shell_path, proxy.as_slice()).map_err(shim_io)?;
  }

  let tar_bytes = builder.into_inner().map_err(|source| oci_layer::Error::Io {
    context: "finishing shim layer".into(),
    source,
  })?;
  let digest = oci_layer::Digest::of(&tar_bytes);
  Ok(Some((digest, tar_bytes)))
}

fn ensure_parent_dirs<W: Write>(builder: &mut tar::Builder<W>, created: &mut BTreeSet<PathBuf>, path: &Path) -> oci_layer::Result<()> {
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
      builder.append_data(&mut header, &dir, std::io::empty()).map_err(|source| oci_layer::Error::Io {
        context: "writing shim layer".into(),
        source,
      })?;
    }
  }
  Ok(())
}

/// Finds `/.needy/proxy` bytes, checking snapshot layers top-down first and
/// then base image layers top-down.
fn find_proxy_bytes(base: &BaseArchive, indexes: &[Index]) -> oci_layer::Result<Vec<u8>> {
  let proxy_str = PROXY.trim_start_matches('/');
  let proxy_rel: PathBuf = proxy_str.split('/').collect();
  for index in indexes.iter().rev() {
    if index.entries.contains_key(&proxy_rel)
      && let Kind::File(data) = index.resolve(&proxy_rel)?.kind
    {
      let mut bytes = Vec::with_capacity(data.size as usize);
      data.reader().read_to_end(&mut bytes).map_err(|source| oci_layer::Error::Io {
        context: "reading /.needy/proxy from layer".into(),
        source,
      })?;
      return Ok(bytes);
    }
  }

  if let Some(bytes) = base.find_regular_file_top_down(proxy_str)? {
    return Ok(bytes);
  }

  Err(oci_layer::Error::Invalid(format!("base image {} has no {PROXY}; cannot shim shells", base.path().display())))
}

fn is_shim_candidate(path: &Path) -> bool {
  let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
    return false;
  };
  if !SHIM_NAMES.contains(&name) {
    return false;
  }
  let Some(parent) = path.parent().and_then(normalize_tar_path) else {
    return false;
  };
  SHIM_DIRS.contains(&parent.as_str())
}

/// Whether a symlink at `shell_path` pointing to `target` resolves inside one
/// of [`SHIM_DIRS`] (in which case `images/container/shim.sh` leaves the
/// symlink alone, because its target is already a shim or real program in a
/// shimmed directory).
fn symlink_stays_in_shim_dirs(shell_path: &Path, target: &Path) -> bool {
  // Container symlinks are POSIX paths: check `Component::RootDir` rather than
  // `Path::is_absolute` so `/opt/...` is treated as absolute on Windows too.
  let is_posix_abs = matches!(target.components().next(), Some(Component::RootDir));
  let combined = if is_posix_abs {
    target.to_path_buf()
  } else {
    shell_path.parent().unwrap_or(Path::new("")).join(target)
  };
  let mut parts: Vec<String> = Vec::new();
  for component in combined.components() {
    match component {
      Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
      Component::ParentDir => {
        parts.pop();
      }
      Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
    }
  }
  if parts.len() < 2 {
    return false;
  }
  let parent = parts[..parts.len() - 1].join("/");
  SHIM_DIRS.contains(&parent.as_str())
}

#[cfg(test)]
#[path = "oci_test.rs"]
mod tests;
