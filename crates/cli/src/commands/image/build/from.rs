//! `needy image build --from REF`: a VM image made from an OCI image `FROM`
//! needy's container base, for this host's VM driver (`linux-firecracker`,
//! or `macos-vz` on Apple silicon). Its root file system is the OCI image's,
//! with the container shims put back as their real programs, as BuildKit
//! builds it (`from.Dockerfile`); its kernel, and Firecracker or the
//! initramfs, are the base VM image's. It is written where a profile whose
//! `actor.image` is `REF` finds it (`needy_core_actor::image::vm::dir_of`).
//!
//! The OCI image is checked first: it must record this needy's base ABI
//! (the `needy.base-abi` label it inherits from the base) and be of the VM's
//! platform; one the engine does not have is pulled for that platform. The
//! new image's `manifest.json` records the reference, the image's ID and
//! platform.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use bollard::Docker;
use bollard::query_parameters::{BuildImageOptions, CreateImageOptionsBuilder, ImageBuildOutput};
use futures_util::TryStreamExt as _;
use needy_config::profile::ActorDriver;
use needy_core_actor::image::manifest::{self, Manifest};
use needy_core_actor::image::{base, container, vm};

use crate::output;

/// The Dockerfile that makes the root file system.
const DOCKERFILE: &str = include_str!("from.Dockerfile");

/// What `--from` builds: the OCI image `reference`, for this host's VM
/// driver, from the base VM image in `base`, into `out`.
pub struct From<'a> {
  pub reference: &'a str,
  pub base: &'a Path,
  /// Where needy's VM images are, for the default output directory.
  pub images: &'a Path,
  pub out: Option<&'a Path>,
}

/// Builds the VM image and returns its directory.
pub async fn build(docker: &Docker, from: &From<'_>) -> Result<PathBuf> {
  let driver = vm::host_driver();
  let arch = vm::arch(driver);
  let platform = format!("linux/{}", container::oci_arch(arch));
  // The base first: it is local, and its kernel is what the image boots.
  check_base(driver, from.base)?;
  let image = inspect(docker, from.reference, &platform).await?;
  base::check(
    from.reference,
    image.config.as_ref().and_then(|c| c.labels.as_ref()).and_then(|l| l.get(base::LABEL)).map(String::as_str),
    &base::rebuild_from("needy image build --linux-container", None),
  )?;
  let (os, architecture) = (image.os.clone().unwrap_or_default(), image.architecture.clone().unwrap_or_default());
  container::check_platform(from.reference, &os, &architecture, arch).map_err(anyhow::Error::msg)?;

  let out = match from.out {
    Some(out) => out.to_path_buf(),
    None => vm::dir_of(from.images, driver, from.reference),
  };
  let parent = out.parent().context("the output directory has no parent")?;
  std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
  // Written next to the output and renamed into place, so a daemon never
  // boots a half-written image.
  let staging = parent.join(format!(".needy-build-{}", uuid::Uuid::new_v4()));
  let built = write(docker, from, driver, &platform, &image, &staging).await;
  let placed = built.and_then(|()| {
    if out.exists() {
      std::fs::remove_dir_all(&out).with_context(|| format!("cannot replace {}", out.display()))?;
    }
    std::fs::rename(&staging, &out).with_context(|| format!("cannot move the image to {}", out.display()))
  });
  if placed.is_err()
    && let Err(e) = std::fs::remove_dir_all(&staging)
    && e.kind() != std::io::ErrorKind::NotFound
  {
    tracing::warn!("cannot remove {}: {e}", staging.display());
  }
  placed?;
  Ok(out)
}

/// Fails unless `dir` holds a complete base VM image of `driver` of this
/// needy's base ABI.
fn check_base(driver: ActorDriver, dir: &Path) -> Result<()> {
  let checked = match driver {
    ActorDriver::MacosVz => vm::MacosVz::new(dir).check(),
    _ => vm::LinuxFirecracker::new(dir).check(),
  };
  checked.context("the base VM image, whose kernel the new image boots")
}

/// What the engine has of `reference`, pulled for `platform` if it has none.
async fn inspect(docker: &Docker, reference: &str, platform: &str) -> Result<bollard::models::ImageInspect> {
  match docker.inspect_image(reference).await {
    Ok(image) => return Ok(image),
    Err(bollard::errors::Error::DockerResponseServerError {
      status_code: 404, ..
    }) => {}
    Err(e) => return Err(e).with_context(|| format!("cannot inspect {reference}")),
  }
  output::message(format_args!("build: pulling {reference} for {platform}"));
  let options =
    CreateImageOptionsBuilder::default().from_image(&needy_core_executor::engine::with_tag(reference)).platform(platform).build();
  docker
    .create_image(Some(options), None, None)
    .try_collect::<Vec<_>>()
    .await
    .with_context(|| format!("{reference} is not here, and cannot be pulled for {platform}"))?;
  docker.inspect_image(reference).await.with_context(|| format!("cannot inspect {reference}"))
}

/// Writes the VM image into the new directory `dir`: the root file system
/// built from `image`, the base's other files and the manifest.
async fn write(
  docker: &Docker,
  from: &From<'_>,
  driver: ActorDriver,
  platform: &str,
  image: &bollard::models::ImageInspect,
  dir: &Path,
) -> Result<()> {
  std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
  let archive = dir.join(".needy-build.tar");
  super::build(
    docker,
    &context()?,
    BuildImageOptions {
      dockerfile: "Dockerfile".into(),
      target: "rootfs".into(),
      platform: platform.into(),
      buildargs: Some(HashMap::from([("FROM".to_string(), from.reference.to_string())])),
      outputs: Some(ImageBuildOutput::Tar(archive.display().to_string())),
      ..super::options()
    },
  )
  .await?;
  let (unpack_from, unpack_to) = (archive.clone(), dir.to_path_buf());
  tokio::task::spawn_blocking(move || super::unpack(&unpack_from, &unpack_to)).await??;
  std::fs::remove_file(&archive).with_context(|| format!("cannot remove {}", archive.display()))?;
  for file in vm::base_files(driver) {
    let (source, target) = (from.base.join(file), dir.join(file));
    std::fs::copy(&source, &target).with_context(|| format!("cannot copy {} to {}", source.display(), target.display()))?;
  }
  // The image's label was checked equal to this needy's base ABI.
  let source = manifest::Source::Oci {
    reference: needy_core_executor::engine::with_tag(from.reference),
    id: image.id.clone().unwrap_or_default(),
    platform: platform.to_string(),
  };
  Ok(manifest::write(dir, &Manifest::made(driver, base::ABI, source))?)
}

/// The build context: the Dockerfile alone.
fn context() -> Result<Vec<u8>> {
  let mut tar = tar::Builder::new(Vec::new());
  let mut header = tar::Header::new_gnu();
  header.set_size(DOCKERFILE.len() as u64);
  header.set_mode(0o644);
  header.set_cksum();
  tar.append_data(&mut header, "Dockerfile", DOCKERFILE.as_bytes())?;
  Ok(tar.into_inner()?)
}
