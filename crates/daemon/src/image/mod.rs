//! Image sessions and building: resolving an image definition's build plan,
//! running build steps in disposable VM sessions with single-flight
//! deduplication per cache key, interactive edit sessions with local exec
//! recording and commit collapse, commit rollback, and materializing the
//! resulting snapshot for `run create`.
//!
//! See `docs/ai/references/architecture/2026-11-17-images-and-run-data-spec.md`
//! ("Resolving and planning a build", "The layer store", "Building and editing").

mod build;
mod container;
mod edit;
mod prune;
mod session;
mod single_flight;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use needy_api_server::control::{
  Chunks, ControlError, GuestSession, ImageBuild, ImageBuildResult, ImageEditFinished, ImageEditStart, ImageEditStarted, ImageExport,
  ImageLog, ImageLogResult, ImagePrune, ImagePruneResult, ImageRollback, ImageRollbackResult, Images as ImagesTrait,
};
use needy_common::sync::lock;
use needy_config::image::{Definition, Image};
use needy_config::profile::{ActorDriver, CONTAINER_IMAGE, User};
use needy_core::image::Digest;
use needy_core_actor::image::feature::FeatureStore;
use needy_core_actor::image::store::registry::Registry;
use needy_core_actor::image::store::{Store, snapshot_tag};
use needy_trace::Dispatch;

pub(crate) use self::build::base_root;
#[cfg(test)]
pub(crate) use self::build::root_digest;
pub(crate) use self::build::select_builder_driver;
use self::build::{CachedRootDigest, build_image_plan, resolve_image_snapshot, resolve_remote_snapshot};
#[cfg(test)]
use self::build::{FAILURE_LOG_MAX_BYTES, feature_install_script, format_build_failure, resolve_features};
use self::container::{export_for_apple_container, export_for_linux_container, stream_export_file};
pub(crate) use self::container::{materialize_for_linux_container, stream_archive};
pub(crate) use self::edit::EditSession;
use self::session::session_profile_yaml;
pub(crate) use self::session::{SessionSpec, run_session};
use self::single_flight::SingleFlight;
use crate::holder::ContainerBackends;
use crate::run::{Boot, Run};
use crate::{Daemon, actor};

/// The daemon's layer store, cache-key index, single-flight build table, and
/// active interactive edit sessions.
pub struct Images {
  /// The content-addressed layer and snapshot store.
  pub store: Store,
  /// OCI Dev Container feature fetcher and content-addressed cache.
  features: Mutex<FeatureStore>,
  /// Maps `needy_core::image::Digest` cache keys to built layer digests
  /// (`sha256-<hex>` files containing `sha256:<layer-hex>\n`).
  keys_dir: PathBuf,
  /// Where the layer registry holds a cache key's layer, as this daemon
  /// pulled or pushed it (`sha256-<hex>.json`, named as in `keys_dir`), so a
  /// remote actor runner is told where to pull it without a lookup per run.
  sources_dir: PathBuf,
  /// Cached exported base OCI image tars keyed by base image ID.
  base_oci_dir: PathBuf,
  /// Container runtime boundaries (Docker-compatible engine and Apple's `container` CLI).
  backends: ContainerBackends,
  /// The layer registry (`daemon.layer_registry`): a step missing here is
  /// pulled from it before it is built, and a built step is pushed to it.
  registry: Option<Registry>,
  /// In-flight builds keyed by step cache key, so concurrent `run create`s
  /// build each missing cache key once and wait for the same result.
  step_flight: SingleFlight<Digest, Digest>,
  /// In-flight base container image exports into `base-oci/`.
  base_export_flight: SingleFlight<String, PathBuf>,
  /// In-flight container snapshot materializations and runtime loads.
  materialize_flight: SingleFlight<String, String>,
  /// Image names with an active edit session: enforces one edit session per
  /// image at a time.
  active_edits: Mutex<HashSet<String>>,
  /// Live edit sessions keyed by session ID, for terminal attach and
  /// `FinishImageEdit`.
  edits: Mutex<HashMap<String, Arc<EditSession>>>,
  /// Cached SHA-256 of a locally built base `rootfs.ext4` (which has no
  /// `manifest.sha256`), so repeated `run create` / `image build` calls do not
  /// re-hash hundreds of megabytes on every invocation.
  root_digest_cache: Mutex<Option<CachedRootDigest>>,
}

/// The layer store under `cache_dir` (built and fetched layers, disks,
/// snapshots) and `data_dir` (commit layers and commit history).
pub(crate) fn store_in(cache_dir: &Path, data_dir: &Path) -> Store {
  Store::new(cache_dir.join("image-store"), data_dir.join("image-store"))
}

impl Images {
  /// Opens the layer store under `cache_dir` (built layers, disks, snapshots,
  /// cache keys) and `data_dir` (commit layers and commit history), sharing
  /// built layers through `registry` when there is one.
  pub(crate) fn with_backends(cache_dir: &Path, data_dir: &Path, backends: ContainerBackends, registry: Option<Registry>) -> Self {
    let cache = cache_dir.join("image-store");
    let keys_dir = cache.join("keys");
    let sources_dir = cache.join("registry-sources");
    let base_oci_dir = cache.join("base-oci");
    let features = FeatureStore::new(cache_dir).expect("cannot initialize HTTP client for feature store");
    Self {
      store: store_in(cache_dir, data_dir),
      features: Mutex::new(features),
      keys_dir,
      sources_dir,
      base_oci_dir,
      backends,
      registry,
      step_flight: SingleFlight::default(),
      base_export_flight: SingleFlight::default(),
      materialize_flight: SingleFlight::default(),
      active_edits: Mutex::default(),
      edits: Mutex::default(),
      root_digest_cache: Mutex::default(),
    }
  }

  /// Replaces the feature store (for tests pointing at a local mock OCI registry).
  #[cfg(test)]
  pub(crate) fn set_feature_store(&self, store: FeatureStore) {
    *lock(&self.features) = store;
  }

  fn feature_store(&self) -> FeatureStore {
    lock(&self.features).clone()
  }

  /// Container runtime boundaries used by container actor drivers and image
  /// materialization.
  pub(crate) fn backends(&self) -> &ContainerBackends {
    &self.backends
  }

  /// The layer registry (`daemon.layer_registry`), if one is configured.
  pub(crate) fn registry(&self) -> Option<&Registry> {
    self.registry.as_ref()
  }
}

/// Resolves `run`'s `actor.image` when it is an [`Image::Definition`]: builds
/// any missing `run` layers, stacks any lockfile commit on top, materializes
/// the resulting snapshot for the run's driver, and installs it on `run` via
/// [`Run::set_boot`]. No-op when `actor.image` is absent or an OCI image
/// reference.
// TODO(images): auto-pulling a missing base VM image on `run create` comes
// with slice 3 of the images and run data spec ("Slices"); until then
// `actor::check_host` fails before this runs with the `needy image pull`
// advice.
pub(crate) async fn prepare(daemon: &Daemon, run: &Run) -> Result<()> {
  let Some(Image::Definition(definition)) = &run.profile.actor.image else {
    return Ok(());
  };
  let resolved = build_image_plan(
    daemon,
    definition,
    run.profile.actor.driver,
    &run.profile.user,
    run.image_commit.as_ref(),
    &run.pinned_features,
    &run.trace,
    &run.cancel,
  )
  .await?;

  let commit_rebased = resolved.plan.commit_rebased();
  run.set_boot(Boot {
    snapshot: Some(resolved.snapshot),
    root: Some(resolved.plan.root),
    layer_disks: resolved.layer_disks,
    snapshot_image: resolved.snapshot_image,
    path: resolved.plan.path,
    env: resolved.plan.env,
    session: GuestSession::default(),
    commit_rebased,
    resolved_features: resolved.resolved_features,
    start_hooks: resolved.plan.hooks.start,
    stop_hooks: resolved.plan.hooks.stop,
    sync_files: resolved.plan.sync_files,
    sources: Default::default(),
  });
  Ok(())
}

/// Resolves `run`'s `actor.image` as [`prepare`] does for an actor on a
/// remote actor runner: plans it, takes its layers from the layer store or
/// builds them here, and installs the snapshot and its root on `run`
/// without materializing disks, which the runner makes of the layers it
/// fetches (`GuestService.FetchLayer`). A layer this daemon neither holds
/// nor can build fails the create before the actor starts, never booting
/// the base alone. No-op when `actor.image` is absent or an OCI image
/// reference.
// TODO(images): an OCI image reference for a remote actor is ignored here
// as for a local VM actor; see `prepare`.
pub(crate) async fn prepare_remote(daemon: &Daemon, run: &Run) -> Result<()> {
  let Some(Image::Definition(definition)) = &run.profile.actor.image else {
    return Ok(());
  };
  let resolved =
    resolve_remote_snapshot(daemon, definition, &run.profile.user, run.image_commit.as_ref(), &run.pinned_features, &run.trace, &run.cancel)
      .await?;

  let commit_rebased = resolved.plan.commit_rebased();
  run.set_boot(Boot {
    snapshot: Some(resolved.snapshot),
    root: Some(resolved.plan.root),
    layer_disks: Vec::new(),
    snapshot_image: None,
    sources: resolved.sources,
    path: resolved.plan.path,
    env: resolved.plan.env,
    session: GuestSession::default(),
    commit_rebased,
    resolved_features: resolved.resolved_features,
    start_hooks: resolved.plan.hooks.start,
    stop_hooks: resolved.plan.hooks.stop,
    sync_files: resolved.plan.sync_files,
  });
  Ok(())
}

#[async_trait::async_trait]
impl ImagesTrait for Daemon {
  async fn build(&self, request: ImageBuild) -> Result<ImageBuildResult, ControlError> {
    self.admit_work()?;
    let definition = parse_definition(&request.image_yaml)?;
    let target_driver = parse_driver(&request.driver)?;
    let user = request.user.unwrap_or_default();
    let base_container_image = definition.root.as_deref().unwrap_or(CONTAINER_IMAGE);
    let builder = select_builder_driver(self, target_driver).await;
    check_driver_host(self, builder, base_container_image).await?;
    if target_driver != builder {
      check_driver_host(self, target_driver, base_container_image).await?;
    }

    let trace = Dispatch::new(
      "image-build",
      Vec::new(),
      needy_trace::Options {
        capacity: 64,
        version: needy_core::event::SCHEMA_VERSION,
        timing: false,
      },
    );
    let resolved =
      build_image_plan(self, &definition, target_driver, &user, request.commit.as_ref(), &request.pinned_features, &trace, &self.cancel)
        .await
        .map_err(crate::failed)?;

    Ok(ImageBuildResult {
      snapshot: resolved.snapshot.digest.to_string(),
      commit_rebased: resolved.plan.commit_rebased(),
      steps: resolved.steps,
      resolved_features: resolved.resolved_features,
    })
  }

  async fn start_edit(&self, request: ImageEditStart) -> Result<ImageEditStarted, ControlError> {
    edit::start_edit(self, request).await
  }

  async fn finish_edit(&self, session_id: &str, abort: bool) -> Result<ImageEditFinished, ControlError> {
    edit::finish_edit(self, session_id, abort).await
  }

  async fn rollback(&self, request: ImageRollback) -> Result<ImageRollbackResult, ControlError> {
    edit::rollback(self, request).await
  }

  async fn log(&self, request: ImageLog) -> Result<ImageLogResult, ControlError> {
    edit::log(self, request).await
  }

  async fn export(&self, request: ImageExport) -> Result<Chunks, ControlError> {
    self.admit_work()?;
    let definition = parse_definition(&request.image_yaml)?;
    let target_driver = parse_driver(&request.driver)?;
    let user = request.user.unwrap_or_default();
    let base_container_image = definition.root.as_deref().unwrap_or(CONTAINER_IMAGE);
    let builder = select_builder_driver(self, target_driver).await;
    check_driver_host(self, builder, base_container_image).await?;
    if target_driver != builder {
      check_driver_host(self, target_driver, base_container_image).await?;
    }

    let trace = Dispatch::new(
      "image-export",
      Vec::new(),
      needy_trace::Options {
        capacity: 64,
        version: needy_core::event::SCHEMA_VERSION,
        timing: false,
      },
    );
    let resolved = resolve_image_snapshot(
      self,
      &definition,
      target_driver,
      &user,
      request.commit.as_ref(),
      &request.pinned_features,
      &trace,
      &self.cancel,
    )
    .await
    .map_err(crate::failed)?;

    let tag = request.tag.unwrap_or_else(|| snapshot_tag(&resolved.snapshot.digest));
    let tar_path = match target_driver {
      ActorDriver::LinuxContainer => {
        export_for_linux_container(self, &resolved.snapshot, base_container_image, &tag).await.map_err(crate::failed)?
      }
      ActorDriver::MacosAppleContainer => {
        export_for_apple_container(self, &resolved.snapshot, base_container_image, &tag).await.map_err(crate::failed)?
      }
      ActorDriver::LinuxFirecracker | ActorDriver::MacosVz => {
        let store = self.images.store.clone();
        let snapshot = resolved.snapshot;
        tokio::task::spawn_blocking(move || store.export_snapshot(&snapshot, &tag))
          .await
          .map_err(|e| ControlError::Internal(format!("exporting image snapshot panicked: {e}")))?
          .map_err(|e| ControlError::Failed(e.to_string()))?
      }
    };

    stream_export_file(&tar_path).await.map_err(crate::failed)
  }

  async fn prune(&self, request: ImagePrune) -> Result<ImagePruneResult, ControlError> {
    prune::prune_images(self, request).await
  }
}

fn parse_definition(yaml: &str) -> Result<Definition, ControlError> {
  needy_config::image::load(&needy_config::Source::text(yaml.to_string(), PathBuf::new()))
    .map_err(|e| ControlError::Invalid(format!("invalid image definition: {e}")))
}

fn parse_driver(name: &str) -> Result<ActorDriver, ControlError> {
  if name.is_empty() {
    return Ok(needy_core_actor::image::vm::host_driver());
  }
  ActorDriver::ALL.into_iter().find(|d| d.as_str() == name).ok_or_else(|| ControlError::Invalid(format!("{name:?} is not an actor driver")))
}

async fn check_driver_host(daemon: &Daemon, driver: ActorDriver, base_container_image: &str) -> Result<(), ControlError> {
  let container_image = matches!(driver, ActorDriver::LinuxContainer | ActorDriver::MacosAppleContainer).then_some(base_container_image);
  let profile_yaml = session_profile_yaml(driver, &User::default(), container_image);
  let profile =
    needy_config::profile::load(&needy_config::Source::text(profile_yaml, PathBuf::new())).map_err(|e| crate::failed(e.into()))?;
  let drv = actor::driver_for(daemon, &profile.actor).map_err(crate::failed)?;
  drv.check_host().await.map_err(|e| crate::failed(e.into()))?;
  actor::guest_needy(&daemon.config).map_err(crate::failed)?;
  Ok(())
}

#[cfg(test)]
#[path = "../image_test.rs"]
pub(crate) mod tests;
