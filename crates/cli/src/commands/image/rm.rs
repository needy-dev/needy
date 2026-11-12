//! `needy image rm`: removes VM images from needy's data directory, and with
//! `--guest-disks` the guest disks of guest binaries no longer in use. It
//! never removes a container image unless `--container` asks for it.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use needy_config::profile::ActorDriver;
use needy_core_actor::image::{guest, vm};

use crate::cli::Context;
use crate::output;

#[derive(clap::Args)]
#[command(arg_required_else_help = true)]
pub struct Args {
  /// The VM images to remove, as `needy image ls` names them: `base`, or
  /// the OCI reference a VM image was made from (`needy image build --from
  /// REF`), or its directory's name.
  #[arg(value_name = "IMAGE")]
  images: Vec<String>,
  /// The VM driver whose images they are. Defaults to this host's.
  #[arg(long, value_parser = super::driver)]
  driver: Option<ActorDriver>,
  /// Also remove each IMAGE that is an OCI reference from the engine and
  /// from Apple's `container` image store, where they have it.
  #[arg(long)]
  container: bool,
  /// Remove the guest disks in needy's data directory except the one of the
  /// guest needy this needy uses: the disks of older guest binaries, which
  /// the daemon leaves.
  #[arg(long)]
  guest_disks: bool,
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  let paths = ctx.paths()?;
  let driver = args.driver.unwrap_or_else(vm::host_driver);
  if !super::is_vm(driver) {
    bail!("{driver} boots no VM image; name a VM driver, linux-firecracker or macos-vz, or use --container alone");
  }
  let mut failed = 0;
  for image in &args.images {
    match find(&paths.images(), driver, image) {
      Some(dir) => {
        std::fs::remove_dir_all(&dir).with_context(|| format!("cannot remove {}", dir.display()))?;
        output::message(format_args!("image rm: removed {}", dir.display()));
      }
      None if args.container => {}
      None => {
        output::message(format_args!("image rm: no {driver} VM image {image} (see `needy image ls`)"));
        failed += 1;
      }
    }
    if args.container && image != "base" && remove_container(image).await == 0 {
      output::message(format_args!("image rm: no container image {image}"));
      failed += 1;
    }
  }
  if args.guest_disks {
    for disk in old_guest_disks(&paths.guest_disks(), current_guest_disk().as_deref())? {
      std::fs::remove_file(&disk).with_context(|| format!("cannot remove {}", disk.display()))?;
      output::message(format_args!("image rm: removed the guest disk {}", disk.display()));
    }
  }
  Ok(if failed == 0 { 0 } else { 1 })
}

/// The directory of `driver`'s VM image `image` in `images`: `base`, a
/// directory of that name, or the one made from the OCI reference `image`.
pub fn find(images: &Path, driver: ActorDriver, image: &str) -> Option<PathBuf> {
  // A name with a path separator, `.` or `..` is no directory's name.
  let one_name = matches!(Path::new(image).components().collect::<Vec<_>>()[..], [std::path::Component::Normal(_)]);
  let named = match image {
    "base" => Some(vm::base_dir(images, driver)),
    _ if one_name => Some(images.join(driver.as_str()).join(image)),
    _ => None,
  };
  named.into_iter().chain([vm::dir_of(images, driver, image)]).find(|dir| dir.is_dir())
}

/// The guest disk of the guest needy this needy finds next to itself, as
/// the daemon would (`needy_core_actor::image::guest::find`); `None` when it
/// finds none.
fn current_guest_disk() -> Option<String> {
  use sha2::Digest as _;
  let exe = std::env::current_exe().ok()?;
  let binary = guest::find(None, &exe, guest::arch()).ok()?;
  let mut hasher = sha2::Sha256::new();
  std::io::copy(&mut std::fs::File::open(&binary).ok()?, &mut hasher).ok()?;
  Some(guest::disk_name(&hasher.finalize()))
}

/// The guest disks in `dir` other than `keep`.
pub fn old_guest_disks(dir: &Path, keep: Option<&str>) -> Result<Vec<PathBuf>> {
  let entries = match std::fs::read_dir(dir) {
    Ok(entries) => entries,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
    Err(e) => return Err(e).with_context(|| format!("cannot read {}", dir.display())),
  };
  let mut disks: Vec<PathBuf> = entries
    .filter_map(|entry| entry.ok().map(|entry| entry.path()))
    .filter(|path| path.extension().is_some_and(|extension| extension == "squashfs"))
    .filter(|path| path.file_name().and_then(|name| name.to_str()) != keep)
    .collect();
  disks.sort();
  Ok(disks)
}

/// Removes the container image `reference` from the engine and Apple's
/// store, and says from how many it was removed.
async fn remove_container(reference: &str) -> usize {
  let mut removed = 0;
  if let Ok(docker) = bollard::Docker::connect_with_defaults() {
    match docker.remove_image(reference, None::<bollard::query_parameters::RemoveImageOptions>, None).await {
      Ok(_) => {
        output::message(format_args!("image rm: removed {reference} from the engine"));
        removed += 1;
      }
      Err(e) => tracing::info!("the engine did not remove {reference}: {e}"),
    }
  }
  #[cfg(unix)]
  if cfg!(target_os = "macos") {
    let store = needy_core_actor::driver::macos_apple_container::MacosAppleContainer::new(reference.to_string());
    match store.delete().await {
      Ok(()) => {
        output::message(format_args!("image rm: removed {reference} from Apple's container"));
        removed += 1;
      }
      Err(e) => tracing::info!("{e}"),
    }
  }
  removed
}

#[cfg(test)]
#[path = "rm_test.rs"]
mod tests;
