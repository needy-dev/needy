//! Container-image materialization, base OCI export, container commit, and
//! archive streaming helpers.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use futures_util::StreamExt as _;
use needy_api_server::control::{Chunks, ControlError};
use needy_core::image::Digest;
use needy_core_actor::driver::macos_apple_container::MacosAppleContainer;
use needy_core_actor::image::store::{Snapshot, snapshot_tag};
use needy_core_executor::engine::{self, Engine};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::Daemon;

/// Ensures `base_image` is present in `engine` (pulling if absent) and exports
/// it once into `base-oci/` under single-flight deduplication.
async fn export_linux_base_tar(daemon: &Daemon, engine: &Arc<dyn Engine>, base_image: &str) -> Result<PathBuf> {
  let inspected = match engine.inspect_image(base_image).await.with_context(|| format!("inspecting container base image {base_image}"))? {
    Some(info) => info,
    None => {
      engine.pull(base_image).await.with_context(|| format!("pulling container base image {base_image}"))?;
      engine
        .inspect_image(base_image)
        .await
        .with_context(|| format!("inspecting pulled container base image {base_image}"))?
        .ok_or_else(|| anyhow!("container actor image {base_image} is still missing after pull"))?
    }
  };
  let sanitized = sanitize_image_id(&inspected.id);
  let base_tar = daemon.images.base_oci_dir.join(format!("{sanitized}.tar"));
  if base_tar.exists() {
    return Ok(base_tar);
  }
  daemon
    .images
    .base_export_flight
    .run(format!("linux:{sanitized}"), || async {
      if !base_tar.exists() {
        write_stream_atomic(&base_tar, engine.export_image(base_image))
          .await
          .with_context(|| format!("exporting base container image {base_image}"))?;
      }
      Ok(base_tar.clone())
    })
    .await
}

/// Ensures `base_image` from Apple's container store is saved once into
/// `base-oci/` under single-flight deduplication.
async fn export_apple_base_tar(daemon: &Daemon, apple: &MacosAppleContainer, base_image: &str) -> Result<PathBuf> {
  let id = apple
    .image_id(base_image)
    .await
    .with_context(|| format!("inspecting Apple container base image {base_image}"))?
    .ok_or_else(|| anyhow!("container actor image {base_image} is not in Apple's container store"))?;
  let sanitized = sanitize_image_id(&id);
  let base_tar = daemon.images.base_oci_dir.join(format!("apple-{sanitized}.tar"));
  if base_tar.exists() {
    return Ok(base_tar);
  }
  daemon
    .images
    .base_export_flight
    .run(format!("apple:{sanitized}"), || async {
      if !base_tar.exists() {
        tokio::fs::create_dir_all(&daemon.images.base_oci_dir)
          .await
          .with_context(|| format!("cannot create {}", daemon.images.base_oci_dir.display()))?;
        let staged = daemon.images.base_oci_dir.join(format!(".save-{}.tar", uuid::Uuid::new_v4()));
        if let Err(e) = apple.save(&staged).await {
          let _ = tokio::fs::remove_file(&staged).await;
          return Err(e).with_context(|| format!("saving base image {base_image} from Apple's container store"));
        }
        if !base_tar.exists() {
          tokio::fs::rename(&staged, &base_tar).await.with_context(|| format!("cannot persist {}", base_tar.display()))?;
        } else {
          let _ = tokio::fs::remove_file(&staged).await;
        }
      }
      Ok(base_tar.clone())
    })
    .await
}

/// Materializes `snapshot` on top of `base_image` as a `docker load` / OCI tar
/// tagged as `tag` without loading it into the engine.
pub(super) async fn export_for_linux_container(daemon: &Daemon, snapshot: &Snapshot, base_image: &str, tag: &str) -> Result<PathBuf> {
  let engine = daemon.images.backends().engine.clone();
  let base_tar = export_linux_base_tar(daemon, &engine, base_image).await?;
  let store = daemon.images.store.clone();
  let snapshot_owned = snapshot.clone();
  let tag_owned = tag.to_string();
  let oci_tar = tokio::task::spawn_blocking(move || store.materialize_container(&snapshot_owned, &base_tar, &tag_owned))
    .await
    .context("materializing container snapshot panicked")??;
  Ok(oci_tar)
}

/// Materializes `snapshot` on top of `base_image` from Apple's container store
/// as a `docker load` / OCI tar tagged as `tag` without loading it back.
pub(super) async fn export_for_apple_container(daemon: &Daemon, snapshot: &Snapshot, base_image: &str, tag: &str) -> Result<PathBuf> {
  let apple = MacosAppleContainer::with_cli(base_image.to_string(), daemon.images.backends().apple_cli.clone());
  let base_tar = export_apple_base_tar(daemon, &apple, base_image).await?;
  let store = daemon.images.store.clone();
  let snapshot_owned = snapshot.clone();
  let tag_owned = tag.to_string();
  let oci_tar = tokio::task::spawn_blocking(move || store.materialize_container(&snapshot_owned, &base_tar, &tag_owned))
    .await
    .context("materializing container snapshot panicked")??;
  Ok(oci_tar)
}

/// Streams `path` in 1 MiB chunks for [`needy_api_server::control::Images::export`].
pub(super) async fn stream_export_file(path: &Path) -> Result<Chunks> {
  const EXPORT_CHUNK: usize = 1024 * 1024;
  let file = tokio::fs::File::open(path).await.with_context(|| format!("cannot open {}", path.display()))?;
  let stream = futures_util::stream::try_unfold((file, vec![0u8; EXPORT_CHUNK]), |(mut file, mut buf)| async move {
    let n = file.read(&mut buf).await.map_err(|e| ControlError::Failed(format!("reading exported image archive: {e}")))?;
    if n == 0 {
      Ok(None)
    } else {
      Ok(Some((buf[..n].to_vec(), (file, buf))))
    }
  });
  Ok(Box::pin(stream))
}

/// Materializes `snapshot` on top of `base_image` and loads `needy-snapshot:<hex>`
/// into the Docker-compatible engine if not already present, pulling `base_image`
/// first when absent from the engine.
pub(crate) async fn materialize_for_linux_container(daemon: &Daemon, snapshot: &Snapshot, base_image: &str) -> Result<String> {
  let engine = daemon.images.backends().engine.clone();
  let tag = snapshot_tag(&snapshot.digest);
  // Short-circuit before exporting the base image or assembling an OCI tar
  // when the snapshot tag is already loaded in the engine.
  if !snapshot.layers.is_empty() && engine.inspect_image(&tag).await.with_context(|| format!("inspecting snapshot image {tag}"))?.is_some() {
    return Ok(tag);
  }

  daemon
    .images
    .materialize_flight
    .run(format!("linux:{base_image}:{tag}"), || async {
      if !snapshot.layers.is_empty()
        && engine.inspect_image(&tag).await.with_context(|| format!("inspecting snapshot image {tag}"))?.is_some()
      {
        return Ok(tag.clone());
      }

      let base_tar = export_linux_base_tar(daemon, &engine, base_image).await?;
      let store = daemon.images.store.clone();
      let snapshot_owned = snapshot.clone();
      let tag_for_build = tag.clone();
      let oci_tar = tokio::task::spawn_blocking(move || store.materialize_container(&snapshot_owned, &base_tar, &tag_for_build))
        .await
        .context("materializing container snapshot panicked")??;

      if engine.inspect_image(&tag).await.with_context(|| format!("inspecting snapshot image {tag}"))?.is_none() {
        let stream = stream_file(&oci_tar).await?;
        engine.load_image(stream).await.with_context(|| format!("loading snapshot image {tag} into the engine"))?;
        if engine.inspect_image(&tag).await.with_context(|| format!("verifying loaded snapshot image {tag}"))?.is_none() {
          anyhow::bail!("loaded snapshot archive into the engine, but image {tag} is still not found");
        }
      }
      Ok(tag.clone())
    })
    .await
}

/// Materializes `snapshot` on top of `base_image` and loads `needy-snapshot:<hex>`
/// into Apple's `container` image store if not already present.
pub(super) async fn materialize_for_apple_container(daemon: &Daemon, snapshot: &Snapshot, base_image: &str) -> Result<String> {
  let apple = MacosAppleContainer::with_cli(base_image.to_string(), daemon.images.backends().apple_cli.clone());
  let tag = snapshot_tag(&snapshot.digest);
  if !snapshot.layers.is_empty() && apple.image_id(&tag).await.with_context(|| format!("inspecting snapshot image {tag}"))?.is_some() {
    return Ok(tag);
  }

  daemon
    .images
    .materialize_flight
    .run(format!("apple:{base_image}:{tag}"), || async {
      if !snapshot.layers.is_empty() && apple.image_id(&tag).await.with_context(|| format!("inspecting snapshot image {tag}"))?.is_some() {
        return Ok(tag.clone());
      }

      let base_tar = export_apple_base_tar(daemon, &apple, base_image).await?;
      let store = daemon.images.store.clone();
      let snapshot_owned = snapshot.clone();
      let tag_for_build = tag.clone();
      let oci_tar = tokio::task::spawn_blocking(move || store.materialize_container(&snapshot_owned, &base_tar, &tag_for_build))
        .await
        .context("materializing container snapshot panicked")??;

      if apple.image_id(&tag).await.with_context(|| format!("inspecting snapshot image {tag}"))?.is_none() {
        apple.load(&oci_tar).await.with_context(|| format!("loading snapshot image {tag} into Apple's container store"))?;
      }
      Ok(tag.clone())
    })
    .await
}

/// Commits `container_name` to a temporary image, exports it to `dir/committed.tar`,
/// and always removes the temporary image afterwards.
pub(super) async fn commit_container_to_file(engine: &Arc<dyn Engine>, container_name: &str, dir: &Path) -> Result<PathBuf> {
  let temp_repo = "needy-session-commit";
  let temp_tag = container_name;
  let temp_ref = format!("{temp_repo}:{temp_tag}");
  engine.commit_container(container_name, temp_repo, temp_tag).await.with_context(|| format!("committing container {container_name}"))?;
  let committed_tar = dir.join("committed.tar");
  let exported = write_stream_atomic(&committed_tar, engine.export_image(&temp_ref))
    .await
    .with_context(|| format!("exporting committed image {temp_ref}"));
  if let Err(e) = engine.remove_image(&temp_ref).await {
    tracing::warn!("cannot remove temporary committed image {temp_ref}: {e}");
  }
  exported?;
  Ok(committed_tar)
}

async fn write_stream_atomic(
  dest: &Path,
  mut stream: futures_util::stream::BoxStream<'static, std::result::Result<bytes::Bytes, engine::Error>>,
) -> Result<()> {
  let dir = dest.parent().ok_or_else(|| anyhow!("{} has no parent directory", dest.display()))?;
  tokio::fs::create_dir_all(dir).await.with_context(|| format!("cannot create {}", dir.display()))?;
  let staged = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
  let write_result = async {
    let mut file = tokio::fs::File::create(&staged).await.with_context(|| format!("cannot create {}", staged.display()))?;
    while let Some(chunk) = stream.next().await {
      let bytes = chunk.map_err(|e| anyhow!("{e}"))?;
      file.write_all(&bytes).await.with_context(|| format!("cannot write {}", staged.display()))?;
    }
    file.flush().await.with_context(|| format!("cannot flush {}", staged.display()))?;
    Ok::<(), anyhow::Error>(())
  }
  .await;
  if let Err(e) = write_result {
    let _ = tokio::fs::remove_file(&staged).await;
    return Err(e);
  }
  if !dest.exists() {
    tokio::fs::rename(&staged, dest).await.with_context(|| format!("cannot persist {}", dest.display()))?;
  } else {
    let _ = tokio::fs::remove_file(&staged).await;
  }
  Ok(())
}

async fn stream_file(path: &Path) -> Result<futures_util::stream::BoxStream<'static, std::result::Result<bytes::Bytes, engine::Error>>> {
  let file = tokio::fs::File::open(path).await.with_context(|| format!("cannot open {}", path.display()))?;
  let stream = futures_util::stream::try_unfold((file, vec![0u8; 64 * 1024]), |(mut file, mut buf)| async move {
    let n = file.read(&mut buf).await.map_err(|e| engine::Error::Refused(e.to_string()))?;
    if n == 0 {
      Ok(None)
    } else {
      let chunk = bytes::Bytes::copy_from_slice(&buf[..n]);
      Ok(Some((chunk, (file, buf))))
    }
  });
  Ok(stream.boxed())
}

fn sanitize_image_id(id: &str) -> String {
  let cleaned: String = id
    .chars()
    .map(|c| match c {
      ':' => '-',
      c if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') => c,
      _ => '_',
    })
    .collect();
  if cleaned.is_empty() || cleaned.starts_with('.') {
    Digest::of(id.as_bytes()).to_string().replace(':', "-")
  } else {
    cleaned
  }
}
