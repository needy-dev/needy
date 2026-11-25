//! `actor.image: REF` on a VM driver: the OCI image `REF`, built `FROM`
//! needy's `actor-base`, is imported above the base (B4 of the layered OCI
//! base spec): its layers above `actor-base` go into the layer store, and
//! the run boots the base VM image with them as layer disks, as it boots an
//! image definition's steps. The control daemon imports for a remote actor
//! too, and the runner fetches the layers from it (`GuestService.FetchLayer`).
//!
//! The base the image must start with is the `actor-base` this daemon's
//! base VM image records in its `manifest.json`.

use std::collections::BTreeMap;
use std::time::Instant;

use anyhow::{Context as _, Result};
use needy_api_server::control::GuestSession;
use needy_config::profile::ActorDriver;
use needy_core::event::{Event, ImageImport};
use needy_core_actor::image::store::import::{self, Imported, Reference};
use needy_core_actor::image::vm;

use crate::Daemon;
use crate::run::{Boot, Run};

/// Where the run's layer disks are made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Disks {
  /// On this host, for a local actor.
  Here,
  /// On the remote actor runner, from the layers it fetches.
  Runner,
}

/// Imports `reference` for `run`'s VM actor and installs the snapshot of
/// its layers on `run`; with [`Disks::Here`] it makes their disks too.
pub(super) async fn prepare(daemon: &Daemon, run: &Run, reference: &str, disks: Disks) -> Result<()> {
  let driver = run.profile.actor.driver;
  let started = Instant::now();
  let imported = import(daemon, reference, driver).await;
  let platform = format!("linux/{}", needy_core_actor::image::container::oci_arch(vm::arch(driver)));
  let event = match &imported {
    Ok(imported) => ImageImport {
      reference: reference.to_string(),
      manifest: Some(imported.manifest.to_string()),
      platform: imported.platform.clone(),
      layers: imported.layers.iter().map(ToString::to_string).collect(),
      pulled: imported.pulled as u64,
      skipped: imported.skipped as u64,
      warnings: imported.warnings.clone(),
      ms: started.elapsed().as_millis() as u64,
      error: None,
    },
    Err(e) => ImageImport {
      reference: reference.to_string(),
      manifest: None,
      platform,
      layers: Vec::new(),
      pulled: 0,
      skipped: 0,
      warnings: Vec::new(),
      ms: started.elapsed().as_millis() as u64,
      error: Some(format!("{e:#}")),
    },
  };
  run.emit(&Event::ImageImport(event));
  let imported = imported?;
  for warning in &imported.warnings {
    tracing::warn!("run {}: actor image {reference}: {warning}", run.id);
  }

  let store = daemon.images.store.clone();
  let layers = imported.layers.clone();
  let (snapshot, layer_disks) = tokio::task::spawn_blocking(move || -> Result<_> {
    if layers.is_empty() {
      return Ok((None, Vec::new()));
    }
    let snapshot = store.snapshot(&layers)?;
    let disks = match disks {
      Disks::Here => store.materialize(&snapshot)?,
      Disks::Runner => Vec::new(),
    };
    Ok((Some(snapshot), disks))
  })
  .await
  .context("snapshotting the imported layers panicked")??;

  run.set_boot(Boot {
    snapshot,
    root: Some(imported.root),
    layer_disks,
    snapshot_image: None,
    path: imported.path.unwrap_or_default(),
    env: imported.env,
    session: GuestSession::default(),
    commit_rebased: false,
    resolved_features: BTreeMap::new(),
    start_hooks: Vec::new(),
    stop_hooks: Vec::new(),
    sync_files: Vec::new(),
  });
  Ok(())
}

/// Imports `reference` for an actor of `driver` above the `actor-base` this
/// daemon's base VM image records.
async fn import(daemon: &Daemon, reference: &str, driver: ActorDriver) -> Result<Imported> {
  let parsed = Reference::parse(reference)?;
  let dir = daemon.config.actor_image.clone();
  let base =
    tokio::task::spawn_blocking(move || import::vm_base(&dir, driver)).await.context("reading the base VM image's manifest panicked")??;
  Ok(import::import(&daemon.images.store, &daemon.images.access, &parsed, &base, vm::arch(driver)).await?)
}

#[cfg(test)]
#[path = "import_test.rs"]
pub(crate) mod tests;
