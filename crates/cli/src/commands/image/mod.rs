//! `needy image`: the actor images on this machine.
//!
//! - `pull`: the base images of this needy's release, for the drivers this
//!   host has: a VM image archive from the GitHub release, or the container
//!   base through the engine or Apple's `container`.
//! - `ls`: the VM images in needy's data directory and the needy container
//!   images the engine and Apple's store have.
//! - `build`: a base image from a needy checkout, or a VM image from an OCI
//!   image (`--from`).
//! - `rm`: a VM image, and the guest disks of old guest binaries.
//!
//! VM images live in `images/<driver>/` in needy's data directory, each in a
//! directory with a `manifest.json`
//! (`needy_core_actor::image::manifest`). The daemon never downloads a VM
//! image: a missing one is an error naming `needy image pull`. The container
//! drivers pull a missing container image themselves.
//!
//! See `docs/ai/references/architecture/2026-11-12-actor-images-design.md`.

use anyhow::Result;
use clap::Subcommand;
use needy_config::profile::ActorDriver;

use crate::cli::Context;

mod build;
mod edit;
mod export;
pub(crate) mod lockfile;
mod log;
mod ls;
mod progress;
pub(crate) mod pull;
mod rm;
mod rollback;

#[derive(clap::Args)]
pub struct Args {
  #[command(subcommand)]
  command: Command,
}

impl Args {
  /// Whether this `image` subcommand talks to the control daemon (and
  /// therefore accepts `--context`/`--address`).
  pub fn allows_selection(&self) -> bool {
    match &self.command {
      Command::Edit(_) | Command::Rollback(_) | Command::Log(_) | Command::Export(_) => true,
      Command::Build(args) => args.is_definition_build(),
      Command::Pull(_) | Command::Ls(_) | Command::Rm(_) => false,
    }
  }
}

#[derive(Subcommand)]
enum Command {
  /// Download the base images of this needy's release.
  ///
  /// For the drivers this host has, or those --driver names: a VM image
  /// (linux-firecracker, macos-vz) from the GitHub release, checked against
  /// its SHA-256, or the container base (and the executor image) through
  /// the engine or Apple's `container`.
  Pull(pull::Args),
  /// List VM images and needy's container images.
  ///
  /// The VM images in needy's data directory, and the needy container
  /// images the engine and Apple's `container` have.
  Ls(ls::Args),
  /// Build an image definition, a base image from a needy checkout, or a VM
  /// image from an OCI image (`--from`).
  Build(build::Args),
  /// Open an interactive writable shell in an image definition and commit
  /// changes to its lockfile on exit.
  Edit(edit::Args),
  /// Roll back an image's commit pointer in its lockfile.
  Rollback(rollback::Args),
  /// Show the commit history recorded for an image definition.
  Log(log::Args),
  /// Export an image definition as a Docker/OCI image archive.
  Export(export::Args),
  /// Remove VM images and old guest disks.
  ///
  /// With --guest-disks, the guest disks of guest binaries no longer in use.
  /// Container images are left alone unless --container says so.
  Rm(rm::Args),
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  match args.command {
    Command::Pull(args) => pull::run(args, ctx).await,
    Command::Ls(args) => ls::run(args, ctx).await,
    Command::Build(args) => build::run(args, ctx).await,
    Command::Edit(args) => edit::run(args, ctx).await,
    Command::Rollback(args) => rollback::run(args, ctx).await,
    Command::Log(args) => log::run(args, ctx).await,
    Command::Export(args) => export::run(args, ctx).await,
    Command::Rm(args) => rm::run(args, ctx).await,
  }
}

/// An actor driver by its name, for `--driver`.
fn driver(name: &str) -> Result<ActorDriver, String> {
  ActorDriver::ALL.into_iter().find(|driver| driver.as_str() == name).ok_or_else(|| {
    let names: Vec<&str> = ActorDriver::ALL.iter().map(|driver| driver.as_str()).collect();
    format!("{name:?} is not an actor driver ({})", names.join(", "))
  })
}

/// Whether `driver` boots a VM image from needy's data directory.
fn is_vm(driver: ActorDriver) -> bool {
  matches!(driver, ActorDriver::LinuxFirecracker | ActorDriver::MacosVz)
}

/// The engine's reference of `image` (`ghcr.io/needy-dev/needy-actor` or
/// `-executor`) for release `version`.
fn release_image(image: &str, version: &str) -> String {
  format!("ghcr.io/needy-dev/{image}:v{version}")
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
