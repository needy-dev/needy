//! `needy image build --from REF`: imports the OCI image `REF`, built
//! `FROM` needy's `actor-base`, into the layer store for this host's VM
//! driver (`linux-firecracker`, or `macos-vz` on Apple silicon), as a run
//! whose `actor.image` is `REF` does: only its layers above `actor-base`,
//! pulled from its registry, without a container engine
//! (`needy_core_actor::image::store::import`). A run of `REF` then finds
//! them stored and pulls none.
//!
//! The base the image must start with is the one the base VM image
//! (`--base`, by default the one the daemon boots) records in its
//! `manifest.json`. Registry credentials and TLS are the daemon's: the
//! Docker credential store, `certs.d`, and `daemon.insecure_registries` (or
//! `--insecure-registry`).
//!
//! It writes no VM image: since B-4 of the layered OCI base spec a VM actor
//! boots the base VM image with the imported layers as layer disks. VM
//! images an older needy made with `--from` are not booted any more; `needy
//! image ls` lists them and `needy image rm` removes them.

use std::path::Path;

use anyhow::{Context as _, Result};
use needy_config::profile::ActorDriver;
use needy_core::image::Digest;
use needy_core_actor::image::store::Store;
use needy_core_actor::image::store::import::{self, Imported, Reference};
use needy_core_actor::image::vm;

use crate::output;

/// What `--from` imports: the OCI image `reference`, above the base the
/// base VM image in `base` records, into `store`, reaching the registry
/// through `access`.
pub struct From<'a> {
  pub reference: &'a str,
  pub base: &'a Path,
  pub store: Store,
  pub access: oci_registry::Access,
}

/// What `--from` imported, and the snapshot its layers stack into (`None`
/// when the image has no layers above the base that change a file).
#[derive(Debug)]
pub struct Report {
  pub imported: Imported,
  pub snapshot: Option<Digest>,
}

/// Imports the image for this host's VM driver.
pub async fn import(from: &From<'_>, feedback: bool) -> Result<Report> {
  import_for(vm::host_driver(), from, feedback).await
}

/// Imports the image for `driver`.
pub async fn import_for(driver: ActorDriver, from: &From<'_>, feedback: bool) -> Result<Report> {
  let reference = Reference::parse(from.reference)?;
  // The base first: it is local, and it says what the image must start with.
  let base = {
    let dir = from.base.to_path_buf();
    tokio::task::spawn_blocking(move || import::vm_base(&dir, driver)).await.context("reading the base VM image panicked")?
  }
  .context("the base VM image, above whose actor-base the image is imported")?;
  let progress = output::Progress::new(feedback, format!("Importing {}", from.reference));
  let imported = import::import(&from.store, &from.access, &reference, &base, vm::arch(driver)).await?;
  progress.message("Recording the snapshot");
  let store = from.store.clone();
  let layers = imported.layers.clone();
  let snapshot = tokio::task::spawn_blocking(move || -> Result<Option<Digest>> {
    if layers.is_empty() {
      return Ok(None);
    }
    Ok(Some(store.snapshot(&layers)?.digest))
  })
  .await
  .context("recording the snapshot panicked")??;
  progress.finish();
  Ok(Report { imported, snapshot })
}

/// What the import says to the user on stderr: the layers it found, pulled
/// and skipped, and its warnings. The snapshot goes to stdout.
pub fn summary(report: &Report) -> Vec<String> {
  let imported = &report.imported;
  let mut lines = vec![format!(
    "image build: imported {} ({}, {}): {} layers above actor-base, {} pulled, {} empty or repeated skipped",
    imported.reference,
    imported.manifest,
    imported.platform,
    imported.layers.len(),
    imported.pulled,
    imported.skipped,
  )];
  lines.extend(imported.warnings.iter().map(|warning| format!("warning: {warning}")));
  if report.snapshot.is_none() {
    lines.push(format!("image build: {} adds nothing above actor-base; its actors boot the base", imported.reference));
  }
  lines
}
