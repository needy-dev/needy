//! Garbage collection for the daemon's layer store, step cache keys, base OCI
//! exports, and `needy-snapshot:*` container images.

use std::collections::HashSet;
use std::path::Path;

use needy_api_server::control::{ControlError, ImagePrune, ImagePruneResult};
use needy_common::sync::lock;
use needy_core::image::Digest;
use needy_core_actor::image::store::snapshot_tag;
use needy_core_executor::engine;

use crate::Daemon;

// TODO(images): automatic age- or size-based background GC is deferred until a
// retention policy is defined; manual `needy image prune` is the entry point.

/// Prunes unreferenced cached layers, disks, snapshots, OCI archives, stale
/// cache keys, optional `base-oci/*.tar` exports (`request.all`), and
/// unreferenced `needy-snapshot:*` container images.
///
/// Always keeps snapshots/layers referenced by live runs, active edit sessions,
/// recorded commit histories, and caller-supplied `request.keep` digests.
pub(super) async fn prune_images(daemon: &Daemon, request: ImagePrune) -> Result<ImagePruneResult, ControlError> {
  daemon.admit_work()?;
  if !daemon.images.step_flight.is_empty() || !daemon.images.base_export_flight.is_empty() || !daemon.images.materialize_flight.is_empty() {
    return Err(ControlError::Failed("cannot prune images while an image build or container materialization is in progress".into()));
  }

  let mut keep: Vec<Digest> = Vec::new();
  for run in daemon.runs.live() {
    if let Some(snapshot) = run.boot().snapshot {
      keep.push(snapshot.digest);
    }
  }
  for session in lock(&daemon.images.edits).values() {
    keep.push(session.snapshot);
  }
  for raw in &request.keep {
    let digest: Digest = raw.parse().map_err(|e| ControlError::Invalid(format!("invalid keep digest {raw:?}: {e}")))?;
    keep.push(digest);
  }

  let kept_tags: HashSet<String> = keep.iter().map(snapshot_tag).collect();
  let store = daemon.images.store.clone();
  let keys_dir = daemon.images.keys_dir.clone();
  let sources_dir = daemon.images.sources_dir.clone();
  let base_oci_dir = daemon.images.base_oci_dir.clone();
  let dry_run = request.dry_run;
  let all = request.all;
  let keep_for_store = keep.clone();

  let (layers_removed, mut bytes_reclaimed) = tokio::task::spawn_blocking(move || -> anyhow::Result<(u64, u64)> {
    let collected = store.collect_with(&keep_for_store, dry_run)?;
    let mut bytes = collected.bytes;
    bytes += prune_stale_keys(&store, &keys_dir, &keep_for_store, dry_run)?;
    bytes += prune_stale_sources(&keys_dir, &sources_dir, dry_run)?;
    if all {
      bytes += prune_base_oci(&base_oci_dir, dry_run)?;
    }
    Ok((collected.layers as u64, bytes))
  })
  .await
  .map_err(|e| ControlError::Internal(format!("pruning image store panicked: {e}")))?
  .map_err(crate::failed)?;

  let engine = daemon.images.backends().engine.clone();
  let mut removed_images = Vec::new();
  match engine.list_images("needy-snapshot:*").await {
    Ok(tags) => {
      for tag in tags {
        if !tag.starts_with("needy-snapshot:") || kept_tags.contains(&tag) {
          continue;
        }
        if !dry_run {
          engine.remove_image(&tag).await.map_err(|e| ControlError::Failed(format!("removing container snapshot image {tag}: {e}")))?;
        }
        removed_images.push(tag);
      }
    }
    // A VM-only host has no Docker engine; pruning the layer store still succeeds.
    Err(engine::Error::Unreachable(_)) => {}
    Err(e) => return Err(ControlError::Failed(format!("listing container snapshot images: {e}"))),
  }

  // TODO(images): prune unreferenced `needy-snapshot:*` tags from Apple's
  // `container` store once `macos_apple_container::Cli` exposes image listing.

  bytes_reclaimed = bytes_reclaimed.saturating_add(0);
  Ok(ImagePruneResult {
    layers_removed,
    snapshots_removed: removed_images.len() as u64,
    bytes_reclaimed,
    removed_images,
  })
}

/// Removes `keys/sha256-*` cache index entries whose target layer was removed
/// (or, in `dry_run`, is not protected by `keep`).
fn prune_stale_keys(store: &needy_core_actor::image::store::Store, keys_dir: &Path, keep: &[Digest], dry_run: bool) -> anyhow::Result<u64> {
  let entries = match std::fs::read_dir(keys_dir) {
    Ok(entries) => entries,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
    Err(e) => return Err(e.into()),
  };
  let mut kept_layers: HashSet<Digest> = HashSet::new();
  if dry_run {
    for digest in keep {
      kept_layers.insert(*digest);
      if let Ok(snapshot) = store.resolve(digest) {
        kept_layers.extend(snapshot.layers);
      }
    }
  }
  let mut bytes = 0u64;
  for entry in entries {
    let entry = entry?;
    let path = entry.path();
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
      continue;
    };
    if !name.starts_with("sha256-") {
      continue;
    }
    let stale = match std::fs::read_to_string(&path).ok().and_then(|raw| raw.trim().parse::<Digest>().ok()) {
      Some(layer) if dry_run => !store.has_layer(&layer) || !kept_layers.contains(&layer),
      Some(layer) => !store.has_layer(&layer),
      None => true,
    };
    if stale {
      bytes += measure_or_remove(&path, dry_run)?;
    }
  }
  Ok(bytes)
}

/// Removes `registry-sources/sha256-*.json` records whose cache key entry is
/// gone, so a source is never offered for a layer this daemon no longer
/// indexes.
// NOTICE: runs after `prune_stale_keys`; a dry run, which removes no key,
// counts only sources whose key was already gone. The records are a few
// hundred bytes each.
fn prune_stale_sources(keys_dir: &Path, sources_dir: &Path, dry_run: bool) -> anyhow::Result<u64> {
  let entries = match std::fs::read_dir(sources_dir) {
    Ok(entries) => entries,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
    Err(e) => return Err(e.into()),
  };
  let mut bytes = 0u64;
  for entry in entries {
    let path = entry?.path();
    let Some(key) = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".json")) else {
      continue;
    };
    if key.starts_with("sha256-") && !keys_dir.join(key).exists() {
      bytes += measure_or_remove(&path, dry_run)?;
    }
  }
  Ok(bytes)
}

/// Removes cached base container image exports (`base-oci/*.tar`), leaving
/// in-progress staged files (`.save-*.tar`, `.*.tmp`) untouched.
fn prune_base_oci(base_oci_dir: &Path, dry_run: bool) -> anyhow::Result<u64> {
  let entries = match std::fs::read_dir(base_oci_dir) {
    Ok(entries) => entries,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
    Err(e) => return Err(e.into()),
  };
  let mut bytes = 0u64;
  for entry in entries {
    let entry = entry?;
    let path = entry.path();
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
      continue;
    };
    if name.starts_with('.') || !name.ends_with(".tar") {
      continue;
    }
    bytes += measure_or_remove(&path, dry_run)?;
  }
  Ok(bytes)
}

fn measure_or_remove(path: &Path, dry_run: bool) -> anyhow::Result<u64> {
  let size = match std::fs::metadata(path) {
    Ok(meta) => meta.len(),
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
    Err(e) => return Err(e.into()),
  };
  if !dry_run {
    match std::fs::remove_file(path) {
      Ok(()) => {}
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
      Err(e) => return Err(e.into()),
    }
  }
  Ok(size)
}
