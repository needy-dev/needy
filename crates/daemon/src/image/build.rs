//! Image planning, feature resolution, single-flight step building, cache-key
//! disk index, builder selection, and base rootfs digest caching.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use anyhow::{Context, Result, anyhow, bail};
use needy_api_server::control::{BuildStepOutcome, GuestSession, UpperLayer};
use needy_common::process::Exit;
use needy_common::sync::lock;
use needy_config::image::{Definition, Layer};
use needy_config::profile::{ActorDriver, CONTAINER_IMAGE, User};
use needy_config::value::Glob;
use needy_core::event::{Event, ImageBuildStep};
use needy_core::image::{Commit as PlanCommit, Digest, FeatureMetadata, Input, Plan, ResolvedFeature, Step};
use needy_core::network::Policy;
use needy_core_actor::driver;
use needy_core_actor::image::feature::{FeatureStore, FetchedFeature, Reference as FeatureReference};
use needy_core_actor::image::store::{LayerDisk, Snapshot, snapshot_tag};
use needy_core_actor::image::{manifest, root};
use needy_trace::Dispatch;
use sha2::Digest as _;
use tokio_util::sync::CancellationToken;

use super::Images;
use super::container::{materialize_for_apple_container, materialize_for_linux_container};
use super::session::{SessionSpec, run_session, session_profile_yaml};
use crate::{Daemon, actor};

/// How many trailing non-empty lines of a failed build session's console log
/// to include in the error message.
const FAILURE_LOG_LINES: usize = 20;

/// Maximum byte length of the console tail included in a failed build error
/// message so the gRPC `grpc-message` trailer stays well under HTTP/2's
/// default 16 KiB `MAX_HEADER_LIST_SIZE` even after percent-encoding.
pub(super) const FAILURE_LOG_MAX_BYTES: usize = 4096;

/// Cached SHA-256 of a locally built base `rootfs.ext4`, invalidated when the
/// file's path, length, or modification time changes.
#[derive(Debug, Clone)]
pub(super) struct CachedRootDigest {
  path: PathBuf,
  len: u64,
  modified: Option<SystemTime>,
  digest: Digest,
}

/// Resolved plan, builder driver, built/cached layers (including any commit
/// layer), and materialized snapshot (layer disks for VM drivers, or an OCI
/// image tag for container drivers).
pub(super) struct ResolvedImage {
  pub(super) builder: ActorDriver,
  pub(super) plan: Plan,
  pub(super) snapshot: Snapshot,
  pub(super) layer_disks: Vec<LayerDisk>,
  pub(super) snapshot_image: Option<String>,
  pub(super) steps: Vec<BuildStepOutcome>,
  pub(super) resolved_features: BTreeMap<String, String>,
}

/// All Dev Container features resolved for an image definition (direct layers
/// plus transitive `dependsOn` entries).
pub(super) struct ResolvedFeatures {
  /// Keyed by raw feature reference as written in the definition or `dependsOn`,
  /// for `needy_core::image::plan`.
  pub(super) by_ref: BTreeMap<String, ResolvedFeature>,
  /// Cached feature artifacts keyed by manifest digest, for `build_step`.
  fetched: BTreeMap<Digest, FetchedFeature>,
  /// Canonical feature reference (`registry/repository`) -> `sha256:<hex>`
  /// digest, returned to the CLI for `<file>.lock`.
  pub(super) resolved_digests: BTreeMap<String, String>,
}

/// Resolved plan, builder driver, and recorded snapshot before driver-specific
/// materialization (VM SquashFS disks or container runtime image load).
pub(super) struct ResolvedSnapshot {
  pub(super) builder: ActorDriver,
  pub(super) plan: Plan,
  pub(super) snapshot: Snapshot,
  pub(super) steps: Vec<BuildStepOutcome>,
  pub(super) resolved_features: BTreeMap<String, String>,
}

impl Images {
  /// Looks up a previously built layer for cache key `key`, verifying that the
  /// layer archive is still present in the store.
  fn lookup_cached(&self, key: &Digest) -> Result<Option<Digest>> {
    let path = self.key_path(key);
    let text = match std::fs::read_to_string(&path) {
      Ok(text) => text,
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
      Err(e) => return Err(e).with_context(|| format!("cannot read cache key {}", path.display())),
    };
    let Ok(digest) = text.trim().parse::<Digest>() else {
      tracing::warn!("ignoring malformed cache key entry {}", path.display());
      return Ok(None);
    };
    if self.store.has_layer(&digest) {
      Ok(Some(digest))
    } else {
      Ok(None)
    }
  }

  /// Atomically records that cache key `key` produced `layer`.
  fn record_cached(&self, key: &Digest, layer: &Digest) -> Result<()> {
    std::fs::create_dir_all(&self.keys_dir).with_context(|| format!("cannot create {}", self.keys_dir.display()))?;
    let path = self.key_path(key);
    let contents = format!("{layer}\n");
    needy_common::fs::write_atomic(&path, contents.as_bytes(), None).with_context(|| format!("cannot persist {}", path.display()))?;
    Ok(())
  }

  fn key_path(&self, key: &Digest) -> PathBuf {
    self.keys_dir.join(key.to_string().replace(':', "-"))
  }

  /// Resolves one build step under single-flight deduplication: returns
  /// `(layer_digest, cached, elapsed_ms)`.
  #[allow(clippy::too_many_arguments)]
  async fn resolve_step(
    self: &Arc<Self>,
    daemon: &Daemon,
    builder: ActorDriver,
    base_container_image: &str,
    profile_user: &User,
    fetched_features: &BTreeMap<Digest, FetchedFeature>,
    trace: &Dispatch,
    cancel: &CancellationToken,
    built_below: &[Digest],
    step: &Step,
  ) -> Result<(Digest, bool, u64)> {
    if let Some(layer) = self.lookup_cached(&step.key)? {
      return Ok((layer, true, 0));
    }

    let mut built_ms: Option<u64> = None;
    let layer = self
      .step_flight
      .run_cancellable(step.key, cancel, "the daemon shut down while waiting for an image build", || async {
        if let Some(layer) = self.lookup_cached(&step.key)? {
          return Ok(layer);
        }
        let started = Instant::now();
        let layer =
          self.build_step(daemon, builder, base_container_image, profile_user, fetched_features, trace, cancel, built_below, step).await?;
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.record_cached(&step.key, &layer)?;
        built_ms = Some(elapsed_ms);
        Ok(layer)
      })
      .await?;

    match built_ms {
      Some(ms) => Ok((layer, false, ms)),
      None => Ok((layer, true, 0)),
    }
  }

  /// Runs a single build step in a fresh VM or container session on top of
  /// `built_below`.
  #[allow(clippy::too_many_arguments)]
  async fn build_step(
    &self,
    daemon: &Daemon,
    builder: ActorDriver,
    base_container_image: &str,
    profile_user: &User,
    fetched_features: &BTreeMap<Digest, FetchedFeature>,
    trace: &Dispatch,
    cancel: &CancellationToken,
    built_below: &[Digest],
    step: &Step,
  ) -> Result<Digest> {
    let (step_label, command, env, copy_in) = match &step.input {
      Input::Run(script) => (script.clone(), vec!["sh".into(), "-c".into(), script.clone()], BTreeMap::new(), Vec::new()),
      Input::Feature {
        reference,
        digest,
        env,
      } => {
        let fetched = fetched_features
          .get(digest)
          .ok_or_else(|| anyhow!("resolved feature `{reference}` ({digest}) is missing its cached archive"))?
          .clone();
        let tar_bytes =
          tokio::task::spawn_blocking(move || fetched.uncompressed_tar()).await.context("reading cached feature archive panicked")??;
        let step_env = feature_install_env(profile_user, env);
        let wrapper = feature_install_script(profile_user, env);
        (
          format!("feature: {reference}"),
          vec!["sh".into(), "-c".into(), wrapper],
          step_env,
          vec![("needy-feature".to_string(), "/tmp/needy-feature".to_string(), tar_bytes)],
        )
      }
    };
    let (snapshot, layer_disks, snapshot_image, cleanup_snapshot_tag) = if built_below.is_empty() {
      (None, Vec::new(), None, None)
    } else if builder == ActorDriver::LinuxContainer {
      let store = self.store.clone();
      let layers = built_below.to_vec();
      let snapshot = tokio::task::spawn_blocking(move || store.snapshot(&layers)).await.context("snapshotting parent layers panicked")??;
      let expected_tag = snapshot_tag(&snapshot.digest);
      let existed = daemon.images.backends().engine.inspect_image(&expected_tag).await.ok().flatten().is_some();
      let tag = materialize_for_linux_container(daemon, &snapshot, base_container_image).await?;
      let cleanup = (!existed).then(|| tag.clone());
      (Some(snapshot), Vec::new(), Some(tag), cleanup)
    } else {
      let store = self.store.clone();
      let layers = built_below.to_vec();
      let (snapshot, disks) = tokio::task::spawn_blocking(move || -> Result<_> {
        let snapshot = store.snapshot(&layers)?;
        let disks = store.materialize(&snapshot)?;
        Ok((snapshot, disks))
      })
      .await
      .context("materializing parent layers panicked")??;
      (Some(snapshot), disks, None, None)
    };

    let spec = SessionSpec {
      driver: builder,
      base_container_image: Some(base_container_image.to_string()),
      snapshot,
      layer_disks,
      snapshot_image,
      image_path: String::new(),
      // Spec ("Building and editing"): build steps run as root with routing
      // off and network open and traced through the gateway.
      user: User {
        name: "root".into(),
        uid: 0,
        gid: 0,
      },
      command,
      cwd: "/".into(),
      env,
      copy_in,
      mirror_out: Vec::new(),
      route_names: Vec::new(),
      session: GuestSession {
        upper: UpperLayer::Kept,
        routing: false,
      },
      tty: None,
      wait_for_attach: false,
      network: Policy {
        allow: vec![Glob::new("*")],
        services: BTreeMap::new(),
      },
      trace: trace.clone(),
    };

    let session_result = run_session(daemon, spec, cancel.clone()).await;
    if let Some(temp_tag) = cleanup_snapshot_tag
      && let Err(e) = daemon.images.backends().engine.remove_image(&temp_tag).await
    {
      tracing::warn!("cannot remove temporary intermediate snapshot image {temp_tag}: {e}");
    }
    let outcome = session_result?;
    match outcome.exit {
      Some(Exit {
        code: Some(0),
        signal,
      }) if signal.is_empty() => {
        outcome.layer.ok_or_else(|| anyhow!("build step `{}` exited 0 without uploading an upper layer", first_line(&step_label)))
      }
      Some(Exit {
        code: Some(code),
        signal,
      }) if signal.is_empty() => {
        bail!(format_build_failure(&step_label, &format!("exited with code {code}"), &outcome.console))
      }
      Some(Exit { signal, .. }) if !signal.is_empty() => {
        bail!(format_build_failure(&step_label, &format!("was killed by {signal}"), &outcome.console))
      }
      _ => {
        bail!(format_build_failure(&step_label, "exited before reporting its status", &outcome.console))
      }
    }
  }
}

/// Fetches all `Layer::Feature` entries in `definition` (plus `builtin:sshfs`
/// when `definition` names a custom root without `sshfs`, per decision E8) and
/// their transitive `dependsOn` references until fixed point, honoring
/// `pinned_features` from the lockfile so pinned cached features and built-in
/// features make zero HTTP requests.
pub(super) async fn resolve_features(
  store: &FeatureStore,
  definition: &Definition,
  pinned_features: &BTreeMap<String, String>,
) -> Result<ResolvedFeatures> {
  let mut queue: VecDeque<String> = VecDeque::new();
  if needy_core::image::needs_sshfs(definition) {
    queue.push_back(needy_core::image::BUILTIN_SSHFS.to_string());
  }
  for layer in &definition.layers {
    if let Layer::Feature { reference, .. } = layer {
      queue.push_back(reference.clone());
    }
  }

  let pull_policy = match definition.pull_policy {
    needy_config::image::PullPolicy::IfNotPresent => needy_core_actor::image::feature::PullPolicy::IfNotPresent,
    needy_config::image::PullPolicy::Always => needy_core_actor::image::feature::PullPolicy::Always,
    needy_config::image::PullPolicy::Never => needy_core_actor::image::feature::PullPolicy::Never,
  };
  let mut by_ref: BTreeMap<String, ResolvedFeature> = BTreeMap::new();
  let mut fetched: BTreeMap<Digest, FetchedFeature> = BTreeMap::new();
  let mut resolved_digests: BTreeMap<String, String> = BTreeMap::new();
  // Cycle guard on canonical references in case a `dependsOn` cycle uses
  // different tags for the same feature; `needy_core::image::plan` still
  // reports the cycle error cleanly.
  let mut expanded_canonical: HashSet<String> = HashSet::new();

  while let Some(raw_ref) = queue.pop_front() {
    if by_ref.contains_key(&raw_ref) {
      continue;
    }
    let (canonical, artifact) = if needy_core_actor::image::feature::is_builtin_ref(&raw_ref) {
      let canonical = needy_config::image::untagged(&raw_ref).to_string();
      let artifact = store.fetch_builtin(&raw_ref).with_context(|| format!("resolving built-in feature `{raw_ref}`"))?;
      (canonical, artifact)
    } else {
      let oci_ref = FeatureReference::parse(&raw_ref).with_context(|| format!("invalid feature reference `{raw_ref}`"))?;
      let canonical = oci_ref.canonical();
      let artifact = match pinned_features.get(&canonical) {
        Some(pinned_str) => {
          let pinned_digest = Digest::parse(pinned_str).with_context(|| format!("invalid pinned digest for feature `{canonical}`"))?;
          store
            .fetch_pinned_with_policy(&oci_ref, &pinned_digest, pull_policy)
            .await
            .with_context(|| format!("fetching pinned feature `{raw_ref}` ({pinned_digest})"))?
        }
        None => store.fetch_with_policy(&oci_ref, pull_policy).await.with_context(|| format!("fetching feature `{raw_ref}`"))?,
      };
      (canonical, artifact)
    };
    let metadata =
      FeatureMetadata::parse(&artifact.metadata_json).with_context(|| format!("parsing devcontainer-feature.json for `{raw_ref}`"))?;

    if expanded_canonical.insert(canonical.clone()) {
      for dep_ref in metadata.depends_on.keys() {
        if !by_ref.contains_key(dep_ref) {
          queue.push_back(dep_ref.clone());
        }
      }
    }

    resolved_digests.insert(canonical, artifact.digest.to_string());
    let digest = artifact.digest;
    fetched.insert(digest, artifact);
    by_ref.insert(raw_ref, ResolvedFeature { digest, metadata });
  }

  Ok(ResolvedFeatures {
    by_ref,
    fetched,
    resolved_digests,
  })
}

/// Who builds the steps of a plan that are not in the layer store.
enum Builder {
  /// This host, in sessions of this driver.
  Here(ActorDriver),
  /// Nobody: a missing step fails the plan, for this reason.
  Unavailable(String),
}

/// Plans `definition`, builds any missing `run` or `feature` steps under
/// single-flight deduplication on the host's builder driver (VM driver when
/// available, else `linux-container` on Linux; never Apple's `container`),
/// stacks `commit` on top if present, and records the resulting [`Snapshot`]
/// without materializing driver-specific boot artifacts.
#[allow(clippy::too_many_arguments)]
pub(super) async fn resolve_image_snapshot(
  daemon: &Daemon,
  definition: &Definition,
  target_driver: ActorDriver,
  user: &User,
  commit: Option<&PlanCommit>,
  pinned_features: &BTreeMap<String, String>,
  trace: &Dispatch,
  cancel: &CancellationToken,
) -> Result<ResolvedSnapshot> {
  let base_container_image = definition.root.as_deref().unwrap_or(CONTAINER_IMAGE);
  let builder = select_builder_driver(daemon, target_driver).await;
  let root = root_digest_for(daemon, builder, base_container_image).await?;
  resolve_snapshot_on(daemon, definition, &Builder::Here(builder), root, user, commit, pinned_features, trace, cancel).await
}

/// Plans `definition` for an actor on a remote `linux-firecracker` runner
/// and records its [`Snapshot`] without materializing anything: the runner
/// fetches the layers and makes the disks.
///
/// The plan is made on this daemon's base VM image, whose digest the runner
/// checks against its own. A step not in the layer store is built here only
/// when this host can run `linux-firecracker` sessions; a `linux-container`
/// build would be made on another root. Otherwise the plan fails, naming
/// the step, before any actor starts.
// TODO(remote-build): dispatching build steps to an actor runner that has
// KVM is deferred; see 2026-11-25-remote-actor-images-spec.md (D1).
#[allow(clippy::too_many_arguments)]
pub(super) async fn resolve_remote_snapshot(
  daemon: &Daemon,
  definition: &Definition,
  user: &User,
  commit: Option<&PlanCommit>,
  pinned_features: &BTreeMap<String, String>,
  trace: &Dispatch,
  cancel: &CancellationToken,
) -> Result<ResolvedSnapshot> {
  // A control daemon without KVM still needs the base VM image's files: the
  // image's cache keys and the runner's root check name its digest.
  let root = root_digest(daemon).await.context(
    "a remote actor's image is planned on this control daemon's base VM image; on a control host without KVM, get it with \
     `needy image pull --driver linux-firecracker`",
  )?;
  let builder = match vm_unavailable(daemon, ActorDriver::LinuxFirecracker).await {
    None => Builder::Here(ActorDriver::LinuxFirecracker),
    Some(reason) => Builder::Unavailable(reason),
  };
  resolve_snapshot_on(daemon, definition, &builder, root, user, commit, pinned_features, trace, cancel).await
}

/// Plans `definition` on `root`, takes each step's layer from the store or
/// has `builder` build it, stacks `commit` on top if present, and records
/// the resulting [`Snapshot`]. The result's `builder` is the builder's
/// driver, or `linux-firecracker` when nobody builds.
#[allow(clippy::too_many_arguments)]
async fn resolve_snapshot_on(
  daemon: &Daemon,
  definition: &Definition,
  builder: &Builder,
  root: Digest,
  user: &User,
  commit: Option<&PlanCommit>,
  pinned_features: &BTreeMap<String, String>,
  trace: &Dispatch,
  cancel: &CancellationToken,
) -> Result<ResolvedSnapshot> {
  let base_container_image = definition.root.as_deref().unwrap_or(CONTAINER_IMAGE);
  let feature_store = daemon.images.feature_store();
  let resolved_features = resolve_features(&feature_store, definition, pinned_features).await?;
  let plan = needy_core::image::plan(definition, &root, user, commit, &resolved_features.by_ref)?;
  let mut layers = Vec::with_capacity(plan.steps.len() + usize::from(plan.commit.is_some()));
  let mut steps = Vec::with_capacity(plan.steps.len());

  for step in &plan.steps {
    if cancel.is_cancelled() {
      bail!("the daemon shut down while building the image");
    }
    let step_label = match &step.input {
      Input::Run(script) => script.clone(),
      Input::Feature { reference, .. } => format!("feature: {reference}"),
    };
    let (layer, cached, ms) = match builder {
      Builder::Here(driver) => {
        daemon
          .images
          .resolve_step(daemon, *driver, base_container_image, user, &resolved_features.fetched, trace, cancel, &layers, step)
          .await?
      }
      Builder::Unavailable(reason) => match daemon.images.lookup_cached(&step.key)? {
        Some(layer) => (layer, true, 0),
        None => bail!(
          "image step `{}` (cache key {}) is not in this control daemon's layer store, and this host cannot build it: {reason}; \
           a remote actor boots layers this daemon holds, so build the image here (`needy image build`) once this host can run \
           linux-firecracker",
          first_line(&step_label),
          step.key
        ),
      },
    };
    let event = Event::ImageBuildStep(ImageBuildStep {
      key: step.key.to_string(),
      layer: layer.to_string(),
      script: step_label,
      cached,
      ms,
    });
    trace.emit(event.kind().as_str(), event.raw());
    steps.push(BuildStepOutcome {
      key: step.key.to_string(),
      layer: layer.to_string(),
      cached,
      ms,
    });
    layers.push(layer);
  }

  if let Some(commit) = &plan.commit {
    if !daemon.images.store.has_layer(&commit.layer) {
      bail!(
        "commit layer {} from the image lockfile is not in this daemon's layer store; roll it back or remove it from the lockfile",
        commit.layer
      );
    }
    layers.push(commit.layer);
  }

  let store = daemon.images.store.clone();
  let snapshot = tokio::task::spawn_blocking(move || store.snapshot(&layers)).await.context("snapshotting layers panicked")??;

  Ok(ResolvedSnapshot {
    builder: match builder {
      Builder::Here(driver) => *driver,
      Builder::Unavailable(_) => ActorDriver::LinuxFirecracker,
    },
    plan,
    snapshot,
    steps,
    resolved_features: resolved_features.resolved_digests,
  })
}

/// Plans `definition`, builds any missing `run` or `feature` steps under
/// single-flight deduplication on the host's builder driver, stacks `commit`
/// on top if present, and materializes the snapshot for `target_driver`.
#[allow(clippy::too_many_arguments)]
pub(super) async fn build_image_plan(
  daemon: &Daemon,
  definition: &Definition,
  target_driver: ActorDriver,
  user: &User,
  commit: Option<&PlanCommit>,
  pinned_features: &BTreeMap<String, String>,
  trace: &Dispatch,
  cancel: &CancellationToken,
) -> Result<ResolvedImage> {
  let base_container_image = definition.root.as_deref().unwrap_or(CONTAINER_IMAGE);
  let resolved = resolve_image_snapshot(daemon, definition, target_driver, user, commit, pinned_features, trace, cancel).await?;

  let store = daemon.images.store.clone();
  let (layer_disks, snapshot_image) = match target_driver {
    ActorDriver::LinuxFirecracker | ActorDriver::MacosVz => {
      let snapshot_owned = resolved.snapshot.clone();
      let disks =
        tokio::task::spawn_blocking(move || store.materialize(&snapshot_owned)).await.context("materializing snapshot panicked")??;
      (disks, None)
    }
    ActorDriver::LinuxContainer => {
      let tag = materialize_for_linux_container(daemon, &resolved.snapshot, base_container_image).await?;
      (Vec::new(), Some(tag))
    }
    ActorDriver::MacosAppleContainer => {
      let tag = materialize_for_apple_container(daemon, &resolved.snapshot, base_container_image).await?;
      (Vec::new(), Some(tag))
    }
  };

  Ok(ResolvedImage {
    builder: resolved.builder,
    plan: resolved.plan,
    snapshot: resolved.snapshot,
    layer_disks,
    snapshot_image,
    steps: resolved.steps,
    resolved_features: resolved.resolved_features,
  })
}

/// Selects the driver that runs image build steps and interactive edit
/// sessions (spec: "Building and editing").
///
/// Prefers the host's VM driver (`macos-vz` on Apple silicon,
/// `linux-firecracker` on Linux with KVM). On a Linux host without a usable VM
/// driver, falls back to `linux-container`. Apple's `container` is never used
/// to build.
pub(crate) async fn select_builder_driver(daemon: &Daemon, target: ActorDriver) -> ActorDriver {
  match target {
    ActorDriver::MacosVz => ActorDriver::MacosVz,
    ActorDriver::LinuxFirecracker => {
      if check_vm_available(daemon, ActorDriver::LinuxFirecracker).await {
        ActorDriver::LinuxFirecracker
      } else {
        ActorDriver::LinuxContainer
      }
    }
    ActorDriver::MacosAppleContainer => {
      if check_vm_available(daemon, ActorDriver::MacosVz).await {
        ActorDriver::MacosVz
      } else if check_vm_available(daemon, ActorDriver::LinuxFirecracker).await {
        ActorDriver::LinuxFirecracker
      } else {
        ActorDriver::MacosVz
      }
    }
    ActorDriver::LinuxContainer => {
      if check_vm_available(daemon, ActorDriver::LinuxFirecracker).await {
        ActorDriver::LinuxFirecracker
      } else if check_vm_available(daemon, ActorDriver::MacosVz).await {
        ActorDriver::MacosVz
      } else {
        ActorDriver::LinuxContainer
      }
    }
  }
}

async fn check_vm_available(daemon: &Daemon, driver: ActorDriver) -> bool {
  vm_unavailable(daemon, driver).await.is_none()
}

/// Why this host cannot run `driver`'s sessions, or `None` when it can.
async fn vm_unavailable(daemon: &Daemon, driver: ActorDriver) -> Option<String> {
  let profile_yaml = session_profile_yaml(driver, &User::default(), None);
  let profile = match needy_config::profile::load(&needy_config::Source::text(profile_yaml, PathBuf::new())) {
    Ok(profile) => profile,
    Err(e) => return Some(e.to_string()),
  };
  let drv = match driver::new(&actor::host(daemon), &profile.actor) {
    Ok(drv) => drv,
    Err(e) => return Some(e.to_string()),
  };
  drv.check_host().await.err().map(|e| e.to_string())
}

/// Computes the root digest for `builder`: the base VM image's root for VM
/// builders, or the root of the base container image for `linux-container`
/// builders on Linux hosts without a VM driver. For needy's own base both
/// are the stack digest of the same `actor-base` ([`root::of_container`]).
// TODO(images): `definition.pull_policy` currently governs OCI feature fetches;
// container base image `always`/`never` pull policy is deferred to a follow-up
// slice (container drivers currently always inspect locally and pull if absent).
async fn root_digest_for(daemon: &Daemon, builder: ActorDriver, base_container_image: &str) -> Result<Digest> {
  if builder == ActorDriver::LinuxContainer {
    let engine = daemon.images.backends().engine.clone();
    let inspected = engine
      .inspect_image(base_container_image)
      .await
      .with_context(|| format!("inspecting container base image {base_container_image}"))?
      .ok_or_else(|| anyhow!("container actor image {base_container_image} is not in the engine"))?;
    return Ok(root::of_container(base_container_image, &inspected)?);
  }
  root_digest(daemon).await
}

/// Computes the root digest for the base VM image configured on `daemon`.
///
/// NOTICE: in slice 1, image definitions on VM drivers boot the host's base VM
/// image (`daemon.config.actor_image`) with built layers stacked over it. The
/// root digest is the stack digest of the `actor-base` its `manifest.json`
/// records (B2 of `2026-11-25-layered-oci-base-spec.md`); for an image that
/// records none, `manifest.json`'s `sha256` field when present (released
/// images), or the SHA-256 of `rootfs.ext4` when absent (locally built images
/// and test fixtures), as [`root::of_vm`] says.
// TODO(images): resolving an OCI `definition.root` to its own lower layers
// comes with slice 5 of the images and run data spec.
pub(crate) async fn root_digest(daemon: &Daemon) -> Result<Digest> {
  let dir = daemon.config.actor_image.clone();
  let images = daemon.images.clone();
  tokio::task::spawn_blocking(move || -> Result<Digest> {
    if let Some(digest) = manifest_root(&dir)? {
      return Ok(digest);
    }
    let rootfs = dir.join("rootfs.ext4");
    let meta = std::fs::metadata(&rootfs).with_context(|| format!("cannot stat {} to hash base rootfs", rootfs.display()))?;
    let len = meta.len();
    let modified = meta.modified().ok();
    if let Some(cached) = lock(&images.root_digest_cache).as_ref()
      && cached.path == rootfs
      && cached.len == len
      && cached.modified == modified
    {
      return Ok(cached.digest);
    }
    let digest = hash_rootfs(&rootfs)?;
    *lock(&images.root_digest_cache) = Some(CachedRootDigest {
      path: rootfs,
      len,
      modified,
      digest,
    });
    Ok(digest)
  })
  .await
  .context("hashing base VM image panicked")?
}

/// The root digest of the base VM image in `dir`, as [`root_digest`]
/// computes it, without its cache: a remote actor runner checks the root a
/// snapshot was built on against the base it boots (D4 in
/// `2026-11-25-remote-actor-images-spec.md`). Blocking.
// NOTICE: a locally built base without `manifest.base` or `manifest.sha256`
// is hashed whole on every call; released images record their base.
pub(crate) fn base_root(dir: &std::path::Path) -> Result<Digest> {
  match manifest_root(dir)? {
    Some(digest) => Ok(digest),
    None => hash_rootfs(&dir.join("rootfs.ext4")),
  }
}

/// The root `manifest.json` in `dir` records ([`root::of_vm`]), if any.
fn manifest_root(dir: &std::path::Path) -> Result<Option<Digest>> {
  let Some(manifest) = manifest::read(dir)? else {
    return Ok(None);
  };
  root::of_vm(&manifest).with_context(|| format!("cannot tell the root of the base VM image in {}", dir.display()))
}

fn hash_rootfs(rootfs: &std::path::Path) -> Result<Digest> {
  let mut file = std::fs::File::open(rootfs).with_context(|| format!("cannot open {} to hash base rootfs", rootfs.display()))?;
  let mut hasher = sha2::Sha256::new();
  std::io::copy(&mut file, &mut hasher).with_context(|| format!("cannot hash {}", rootfs.display()))?;
  Ok(Digest::from_bytes(hasher.finalize().into()))
}

fn to_install_user(user: &User) -> devcontainer_features::InstallUser {
  devcontainer_features::InstallUser {
    name: user.name.clone(),
    uid: user.uid,
    gid: user.gid,
    home: None,
  }
}

/// Builds the environment passed to a Dev Container feature's `install.sh`
/// session: delegates to [`devcontainer_features::feature_install_env`].
fn feature_install_env(profile_user: &User, options: &BTreeMap<String, String>) -> BTreeMap<String, String> {
  devcontainer_features::feature_install_env(&to_install_user(profile_user), options)
}

/// Builds the POSIX `sh -c` wrapper for `/tmp/needy-feature`: delegates to
/// [`devcontainer_features::feature_install_script`].
pub(super) fn feature_install_script(profile_user: &User, options: &BTreeMap<String, String>) -> String {
  devcontainer_features::feature_install_script(&to_install_user(profile_user), options, "/tmp/needy-feature")
}

fn first_line(script: &str) -> &str {
  script.lines().find(|line| !line.trim().is_empty()).unwrap_or(script).trim()
}

pub(super) fn format_build_failure(script: &str, status: &str, console: &str) -> String {
  let mut msg = format!("image build step `{}` {status}", first_line(script));
  let lines: Vec<&str> =
    console.lines().map(|line| line.rsplit('\r').next().unwrap_or(line).trim_end()).filter(|line| !line.trim().is_empty()).collect();
  let tail = &lines[lines.len().saturating_sub(FAILURE_LOG_LINES)..];
  if !tail.is_empty() {
    let mut joined = tail.join("\n");
    if joined.len() > FAILURE_LOG_MAX_BYTES {
      let mut start = joined.len() - FAILURE_LOG_MAX_BYTES;
      while !joined.is_char_boundary(start) {
        start += 1;
      }
      joined = format!("...{}", &joined[start..]);
    }
    msg.push('\n');
    msg.push_str(&joined);
  }
  msg
}
