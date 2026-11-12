//! `needy image ls`: the VM images in needy's data directory, one row per
//! image directory (`images/<driver>/<name>/`), and the needy container
//! images the engine and Apple's `container` have: every image with a base
//! ABI label (bases and custom images `FROM` them) and the executor images.

use std::path::{Path, PathBuf};

use anyhow::Result;
use needy_config::profile::ActorDriver;
use needy_core_actor::image::{base, manifest};
use serde::Serialize;

use crate::cli::Context;
use crate::output::{self, OutputFormat, Row};

#[derive(clap::Args)]
pub struct Args {
  #[arg(short = 'o', long, value_enum, default_value_t)]
  output: OutputFormat,
}

/// One image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Image {
  /// The driver that runs it: a VM driver, `linux-container` (the engine's
  /// store), `macos-apple-container` (Apple's), or `executor` for an
  /// executor image in the engine.
  pub driver: String,
  /// `base` for a base VM image, the OCI reference a VM image was made from,
  /// or a container image's reference.
  pub image: String,
  /// The needy version that made a VM image; `None` for a container image.
  pub version: Option<String>,
  /// The base ABI it records, as written; `None` when it records none.
  pub base_abi: Option<String>,
  /// Where it came from: a VM image's manifest's source, or the store a
  /// container image is in.
  pub source: String,
  /// Bytes on disk (a VM image's blocks, so its holes do not count), or the
  /// store's size of a container image.
  pub size: u64,
  /// A VM image's directory.
  pub path: Option<PathBuf>,
}

impl Row for Image {
  const HEADER: &'static [&'static str] = &[
    "DRIVER", "IMAGE", "VERSION", "BASE ABI", "SOURCE", "SIZE", "PATH",
  ];

  fn cells(&self) -> Vec<String> {
    vec![
      self.driver.clone(),
      self.image.clone(),
      self.version.clone().unwrap_or_else(|| "-".into()),
      self.base_abi.clone().unwrap_or_else(|| "-".into()),
      self.source.clone(),
      size(self.size),
      self.path.as_ref().map_or_else(|| "-".into(), |path| path.display().to_string()),
    ]
  }
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  let mut rows = vm_images(&ctx.paths()?.images());
  match engine_images().await {
    Ok(images) => rows.extend(images),
    Err(e) => tracing::info!("no engine images: {e:#}"),
  }
  match apple_images().await {
    Ok(images) => rows.extend(images),
    Err(e) => tracing::info!("no images from Apple's container: {e:#}"),
  }
  output::print(args.output, &rows)?;
  Ok(0)
}

/// The VM images in `images`: every directory `images/<driver>/<name>/` of
/// a VM driver, with what its manifest says. One without a manifest is
/// listed with `no manifest.json` as its source, so `rm` can be pointed at
/// it.
pub fn vm_images(images: &Path) -> Vec<Image> {
  let mut rows = Vec::new();
  for driver in ActorDriver::ALL.into_iter().filter(|driver| super::is_vm(*driver)) {
    let Ok(entries) = std::fs::read_dir(images.join(driver.as_str())) else {
      continue;
    };
    let mut dirs: Vec<PathBuf> = entries
      .filter_map(|entry| entry.ok())
      // Staging and moved-aside directories of a pull or build in progress.
      .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
      .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
      .map(|entry| entry.path())
      .collect();
    dirs.sort();
    for dir in dirs {
      rows.push(vm_image(driver, &dir));
    }
  }
  rows
}

/// The row of the VM image of `driver` in `dir`.
fn vm_image(driver: ActorDriver, dir: &Path) -> Image {
  let name = dir.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
  let read = manifest::read(dir);
  let found = read.as_ref().ok().and_then(Option::as_ref);
  let image = match found.and_then(|manifest| manifest.source.as_ref()) {
    Some(manifest::Source::Oci { reference, .. }) => reference.clone(),
    _ => name,
  };
  let source = match &read {
    Ok(Some(manifest)) => manifest.source.as_ref().map_or_else(|| "-".into(), ToString::to_string),
    Ok(None) => format!("no {}", manifest::FILE),
    Err(e) => e.to_string(),
  };
  Image {
    driver: driver.to_string(),
    image,
    version: found.map(|manifest| manifest.version.clone()),
    base_abi: found.map(|manifest| manifest.base_abi.to_string()),
    source,
    size: disk_usage(dir),
    path: Some(dir.to_path_buf()),
  }
}

/// The bytes the files in `dir` take on disk: allocated blocks on Unix, so a
/// sparse root file system counts what it holds.
fn disk_usage(dir: &Path) -> u64 {
  let Ok(entries) = std::fs::read_dir(dir) else {
    return 0;
  };
  entries
    .filter_map(|entry| entry.ok()?.metadata().ok())
    .map(|metadata| {
      #[cfg(unix)]
      {
        use std::os::unix::fs::MetadataExt as _;
        metadata.blocks() * 512
      }
      #[cfg(not(unix))]
      {
        metadata.len()
      }
    })
    .sum()
}

/// A size for people, in binary units.
fn size(bytes: u64) -> String {
  const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
  let mut value = bytes as f64;
  let mut unit = 0;
  while value >= 1024.0 && unit < UNITS.len() - 1 {
    value /= 1024.0;
    unit += 1;
  }
  if unit == 0 {
    format!("{bytes} B")
  } else {
    format!("{value:.1} {}", UNITS[unit])
  }
}

/// The needy images the engine has: those with a base ABI label (actor
/// bases and the custom images `FROM` them) and the executor images, one
/// row per tag.
async fn engine_images() -> Result<Vec<Image>> {
  use bollard::query_parameters::ListImagesOptionsBuilder;
  use std::collections::HashMap;
  let docker = bollard::Docker::connect_with_defaults()?;
  let mut rows = Vec::new();
  let actor = HashMap::from([("label".to_string(), vec![base::LABEL.to_string()])]);
  let executor = HashMap::from([(
    "reference".to_string(),
    vec![
      needy_core_executor::driver::linux_container::BUILT_IMAGE.split(':').next().unwrap_or_default().to_string(),
      "ghcr.io/needy-dev/needy-executor".to_string(),
    ],
  )]);
  for (filters, driver) in [
    (actor, ActorDriver::LinuxContainer.as_str()),
    (executor, "executor"),
  ] {
    let options = ListImagesOptionsBuilder::default().filters(&filters).build();
    for image in docker.list_images(Some(options)).await? {
      let base_abi = image.labels.get(base::LABEL).cloned();
      let id: String = image.id.trim_start_matches("sha256:").chars().take(12).collect();
      // Untagged images (left by rebuilds) are not listed: nothing names
      // them, and `docker image prune` removes them.
      for tag in image.repo_tags.iter().filter(|tag| *tag != "<none>:<none>") {
        rows.push(Image {
          driver: driver.to_string(),
          image: tag.clone(),
          version: None,
          base_abi: base_abi.clone(),
          source: format!("engine {id}"),
          size: u64::try_from(image.size).unwrap_or(0),
          path: None,
        });
      }
    }
  }
  rows.sort_by(|a, b| (&a.driver, &a.image).cmp(&(&b.driver, &b.image)));
  Ok(rows)
}

/// The images in Apple's `container` image store that record a base ABI.
#[cfg(unix)]
async fn apple_images() -> Result<Vec<Image>> {
  use needy_core_actor::driver::macos_apple_container::MacosAppleContainer;
  if !cfg!(target_os = "macos") {
    return Ok(Vec::new());
  }
  // The image does not matter for listing the store.
  let store = MacosAppleContainer::new(String::new());
  let mut rows: Vec<Image> = store
    .images()
    .await?
    .into_iter()
    .filter(|image| image.base_abi.is_some())
    .map(|image| Image {
      driver: ActorDriver::MacosAppleContainer.to_string(),
      image: image.name,
      version: None,
      base_abi: image.base_abi,
      source: "Apple's container".into(),
      size: image.size,
      path: None,
    })
    .collect();
  rows.sort_by(|a, b| a.image.cmp(&b.image));
  Ok(rows)
}

#[cfg(not(unix))]
async fn apple_images() -> Result<Vec<Image>> {
  Ok(Vec::new())
}

#[cfg(test)]
#[path = "ls_test.rs"]
mod tests;
