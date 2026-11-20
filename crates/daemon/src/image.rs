//! Image sessions and building: resolving an image definition's build plan,
//! running build steps in disposable VM sessions with single-flight
//! deduplication per cache key, interactive edit sessions with local exec
//! recording and commit collapse, commit rollback, and materializing the
//! resulting snapshot for `run create`.
//!
//! See `docs/ai/references/architecture/2026-11-17-images-and-run-data-spec.md`
//! ("Resolving and planning a build", "The layer store", "Building and editing").

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, anyhow, bail};
use needy_api_server::control::{
  BuildStepOutcome, Caller, Chunks, ControlError, FaultAdded, Guest, GuestConfig, GuestControls, GuestSession, GuestUser, ImageBuild,
  ImageBuildResult, ImageEditFinished, ImageEditStart, ImageEditStarted, ImageRollback, ImageRollbackResult, Images as ImagesTrait, Logs,
  NewRun, ProfileCheck, ProfileToCheck, RecordedCommand, Route, RunEvents, RunFaults, RunInfo, Runs, StopMode, Target, UpperLayer,
  WindowSize,
};
use needy_api_server::{Health, Listen, Server};
use needy_common::process::Exit;
use needy_config::image::{Definition, Image};
use needy_config::profile::{ActorDriver, User};
use needy_config::value::Glob;
use needy_core::event::{ActorStop, Event, ExecDecide, ImageBuildStep};
use needy_core::image::{Commit as PlanCommit, Digest as PlanDigest, Input, Plan, Step};
use needy_core::intercept::exec::ExecContext;
use needy_core::network::Policy;
use needy_core::types::{Network, Origin, Outcome, Placement, Principal};
use needy_core_actor::driver;
use needy_core_actor::image::manifest;
use needy_core_actor::image::store::{Command as StoreCommand, Digest as LayerDigest, Error as StoreError, LayerDisk, Snapshot, Store};
use needy_core_actor::sidecar::gateway;
use needy_trace::Dispatch;
use sha2::Digest as _;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::guest::{UPPER_LAYER, keep_upper};
use crate::pool::Instance;
use crate::run::{Boot, Run};
use crate::{Daemon, actor};

/// How long a cancelled build or edit session's VM gets to exit on SIGTERM
/// before SIGKILL.
const SESSION_STOP_GRACE: Duration = Duration::from_secs(3);

/// How many trailing non-empty lines of a failed build session's console log
/// to include in the error message.
const FAILURE_LOG_LINES: usize = 20;

/// Result broadcast to waiters sharing an in-flight build for one cache key.
type FlightResult = std::result::Result<LayerDigest, String>;

/// Result broadcast when an interactive edit session finishes.
type EditOutcomeResult = std::result::Result<ImageEditFinished, String>;

/// Cached SHA-256 of a locally built base `rootfs.ext4`, invalidated when the
/// file's path, length, or modification time changes.
#[derive(Debug, Clone)]
struct CachedRootDigest {
  path: PathBuf,
  len: u64,
  modified: Option<SystemTime>,
  digest: PlanDigest,
}

/// The daemon's layer store, cache-key index, single-flight build table, and
/// active interactive edit sessions.
pub struct Images {
  /// The content-addressed layer and snapshot store.
  pub store: Store,
  /// Maps `needy_core::image::Digest` cache keys to built layer digests
  /// (`sha256-<hex>` files containing `sha256:<layer-hex>\n`).
  keys_dir: PathBuf,
  /// In-flight builds keyed by step cache key, so concurrent `run create`s
  /// build each missing cache key once and wait for the same result.
  in_flight: Mutex<HashMap<PlanDigest, watch::Receiver<Option<FlightResult>>>>,
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

/// A running interactive edit session.
pub(crate) struct EditSession {
  pub id: String,
  channels: driver::Channels,
  cancel: CancellationToken,
  aborted: Arc<AtomicBool>,
  terminal: tokio::sync::OnceCell<Arc<Instance>>,
  result_rx: watch::Receiver<Option<EditOutcomeResult>>,
}

impl EditSession {
  /// Connects to the edit session actor's process service for terminal attach.
  pub(crate) async fn terminal(&self) -> Result<Arc<Instance>, ControlError> {
    self
      .terminal
      .get_or_try_init(|| async {
        let client = actor::connect_terminal(&self.channels, &self.cancel, &format!("edit session {}", self.id)).await?;
        Ok(Instance::actor(&self.id, Arc::new(client)))
      })
      .await
      .cloned()
  }
}

/// Releases an image's per-image edit lock when dropped.
struct EditLockGuard {
  images: Arc<Images>,
  image_name: String,
}

impl Drop for EditLockGuard {
  fn drop(&mut self) {
    self.images.active_edits.lock().unwrap_or_else(|p| p.into_inner()).remove(&self.image_name);
  }
}

impl Images {
  /// Opens the layer store under `cache_dir` (built layers, disks, snapshots,
  /// cache keys) and `data_dir` (commit layers and commit history).
  pub fn new(cache_dir: &Path, data_dir: &Path) -> Self {
    let cache = cache_dir.join("image-store");
    let data = data_dir.join("image-store");
    let keys_dir = cache.join("keys");
    Self {
      store: Store::new(cache, data),
      keys_dir,
      in_flight: Mutex::default(),
      active_edits: Mutex::default(),
      edits: Mutex::default(),
      root_digest_cache: Mutex::default(),
    }
  }

  /// Looks up an active interactive edit session by its session ID.
  pub(crate) fn edit_session(&self, id: &str) -> Option<Arc<EditSession>> {
    self.edits.lock().unwrap_or_else(|p| p.into_inner()).get(id).cloned()
  }

  /// Acquires the per-image edit lock for `image_name`.
  fn lock_edit(self: &Arc<Self>, image_name: &str) -> Result<EditLockGuard, ControlError> {
    let mut active = self.active_edits.lock().unwrap_or_else(|p| p.into_inner());
    if !active.insert(image_name.to_string()) {
      return Err(ControlError::Failed(format!("an edit session is already running for image {image_name}")));
    }
    Ok(EditLockGuard {
      images: self.clone(),
      image_name: image_name.to_string(),
    })
  }

  /// Looks up a previously built layer for cache key `key`, verifying that the
  /// layer archive is still present in the store.
  fn lookup_cached(&self, key: &PlanDigest) -> Result<Option<LayerDigest>> {
    let path = self.key_path(key);
    let text = match std::fs::read_to_string(&path) {
      Ok(text) => text,
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
      Err(e) => return Err(e).with_context(|| format!("cannot read cache key {}", path.display())),
    };
    let Ok(digest) = text.trim().parse::<LayerDigest>() else {
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
  fn record_cached(&self, key: &PlanDigest, layer: &LayerDigest) -> Result<()> {
    std::fs::create_dir_all(&self.keys_dir).with_context(|| format!("cannot create {}", self.keys_dir.display()))?;
    let path = self.key_path(key);
    let staged = self.keys_dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let mut file =
      std::fs::OpenOptions::new().write(true).create_new(true).open(&staged).with_context(|| format!("cannot stage {}", path.display()))?;
    writeln!(file, "{layer}").with_context(|| format!("cannot write {}", path.display()))?;
    file.sync_all().with_context(|| format!("cannot sync {}", path.display()))?;
    drop(file);
    std::fs::rename(&staged, &path).with_context(|| format!("cannot persist {}", path.display()))?;
    Ok(())
  }

  fn key_path(&self, key: &PlanDigest) -> PathBuf {
    self.keys_dir.join(key.to_string().replace(':', "-"))
  }

  /// Resolves one build step under single-flight deduplication: returns
  /// `(layer_digest, cached, elapsed_ms)`.
  async fn resolve_step(
    self: &Arc<Self>,
    daemon: &Daemon,
    driver: ActorDriver,
    trace: &Dispatch,
    cancel: &CancellationToken,
    built_below: &[LayerDigest],
    step: &Step,
  ) -> Result<(LayerDigest, bool, u64)> {
    if let Some(layer) = self.lookup_cached(&step.key)? {
      return Ok((layer, true, 0));
    }

    enum Role {
      Leader(watch::Sender<Option<FlightResult>>),
      Waiter(watch::Receiver<Option<FlightResult>>),
      Cached(LayerDigest),
    }

    let role = {
      let mut in_flight = self.in_flight.lock().unwrap_or_else(|p| p.into_inner());
      if let Some(layer) = self.lookup_cached(&step.key)? {
        Role::Cached(layer)
      } else if let Some(rx) = in_flight.get(&step.key) {
        Role::Waiter(rx.clone())
      } else {
        let (tx, rx) = watch::channel(None);
        in_flight.insert(step.key.clone(), rx);
        Role::Leader(tx)
      }
    };

    match role {
      Role::Cached(layer) => Ok((layer, true, 0)),
      Role::Waiter(mut rx) => {
        let result = tokio::select! {
          () = cancel.cancelled() => bail!("the daemon shut down while waiting for an image build"),
          changed = rx.wait_for(Option::is_some) => match changed {
            Ok(guard) => guard.clone().expect("wait_for guarantees Some"),
            Err(_) => bail!("the in-flight build for {} ended without a result", step.key),
          }
        };
        match result {
          Ok(layer) => Ok((layer, true, 0)),
          Err(msg) => Err(anyhow!(msg)),
        }
      }
      Role::Leader(tx) => {
        // Ensure the in-flight entry is removed even if this future is dropped.
        struct Guard {
          images: Arc<Images>,
          key: PlanDigest,
        }
        impl Drop for Guard {
          fn drop(&mut self) {
            self.images.in_flight.lock().unwrap_or_else(|p| p.into_inner()).remove(&self.key);
          }
        }
        let _guard = Guard {
          images: self.clone(),
          key: step.key.clone(),
        };

        let started = Instant::now();
        let built = self.build_step(daemon, driver, trace, cancel, built_below, step).await;
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        match built {
          Ok(layer) => {
            self.record_cached(&step.key, &layer)?;
            let _ = tx.send(Some(Ok(layer)));
            Ok((layer, false, elapsed_ms))
          }
          Err(error) => {
            let message = format!("{error:#}");
            let _ = tx.send(Some(Err(message)));
            Err(error)
          }
        }
      }
    }
  }

  /// Runs a single build step in a fresh VM session on top of `built_below`.
  async fn build_step(
    &self,
    daemon: &Daemon,
    driver: ActorDriver,
    trace: &Dispatch,
    cancel: &CancellationToken,
    built_below: &[LayerDigest],
    step: &Step,
  ) -> Result<LayerDigest> {
    let Input::Run(script) = &step.input;
    let (snapshot, layer_disks) = if built_below.is_empty() {
      (None, Vec::new())
    } else {
      let store = self.store.clone();
      let layers = built_below.to_vec();
      tokio::task::spawn_blocking(move || -> Result<_> {
        let snapshot = store.snapshot(&layers)?;
        let disks = store.materialize(&snapshot)?;
        Ok((Some(snapshot), disks))
      })
      .await
      .context("materializing parent layers panicked")??
    };

    let spec = SessionSpec {
      driver,
      snapshot,
      layer_disks,
      image_path: String::new(),
      // Spec ("Building and editing"): build steps run as root with routing
      // off and network open and traced through the gateway.
      user: User {
        name: "root".into(),
        uid: 0,
        gid: 0,
      },
      command: vec!["sh".into(), "-c".into(), script.clone()],
      cwd: "/".into(),
      env: BTreeMap::new(),
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

    let outcome = run_session(daemon, spec, cancel.clone()).await?;
    match outcome.exit {
      Some(Exit {
        code: Some(0),
        signal,
      }) if signal.is_empty() => {
        outcome.layer.ok_or_else(|| anyhow!("build step `{}` exited 0 without uploading an upper layer", first_line(script)))
      }
      Some(Exit {
        code: Some(code),
        signal,
      }) if signal.is_empty() => {
        bail!(format_build_failure(script, &format!("exited with code {code}"), &outcome.console))
      }
      Some(Exit { signal, .. }) if !signal.is_empty() => {
        bail!(format_build_failure(script, &format!("was killed by {signal}"), &outcome.console))
      }
      _ => {
        bail!(format_build_failure(script, "exited before reporting its status", &outcome.console))
      }
    }
  }
}

/// Configuration for an actor session (a build step, or an interactive edit
/// session).
pub struct SessionSpec {
  /// Which VM driver to boot.
  pub driver: ActorDriver,
  /// The layers stacked below the session's upper layer, lowest first.
  pub snapshot: Option<Snapshot>,
  /// Materialized SquashFS layer disks of `snapshot`, lowest first.
  pub layer_disks: Vec<LayerDisk>,
  /// The image's `PATH`; empty uses the default system `PATH`.
  pub image_path: String,
  /// The user the session command runs as.
  pub user: User,
  /// The command the guest init runs.
  pub command: Vec<String>,
  /// Working directory inside the actor.
  pub cwd: String,
  /// Environment variables passed to the session command.
  pub env: BTreeMap<String, String>,
  /// Command names the kernel routes; `*` routes every exec.
  pub route_names: Vec<String>,
  /// Session settings (upper layer disposition and exec routing).
  pub session: GuestSession,
  /// Optional terminal size (for interactive edit sessions; `None` for build steps).
  pub tty: Option<WindowSize>,
  /// Whether init waits for a terminal attach before starting the command.
  pub wait_for_attach: bool,
  /// Network policy enforced by the session's gateway.
  pub network: Policy,
  /// Trace that receives the session's gateway network events and guest events.
  pub trace: Dispatch,
}

/// The outcome of a completed actor session.
pub struct SessionOutcome {
  /// The command's exit status as the guest reported it; `None` if the VM
  /// exited before reporting.
  pub exit: Option<Exit>,
  /// The stored layer digest when `session.upper == UpperLayer::Kept` and the
  /// session uploaded an upper layer after exiting cleanly.
  pub layer: Option<LayerDigest>,
  /// The actor's console log (may be empty).
  pub console: String,
}

/// Boots an actor session, serves its guest API and gateway, waits for the
/// actor to exit, and stores its uploaded upper layer when kept.
pub async fn run_session(daemon: &Daemon, spec: SessionSpec, cancel: CancellationToken) -> Result<SessionOutcome> {
  let session_id = format!("s-{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
  let dir = daemon.config.state_dir.join("sessions").join(&session_id);
  needy_api_server::create_private_dir(&dir).with_context(|| format!("cannot create {}", dir.display()))?;

  let result = run_session_in(daemon, &session_id, &dir, spec, cancel).await;
  cleanup_session_dir(&dir);
  result
}

async fn run_session_in(
  daemon: &Daemon,
  session_id: &str,
  dir: &Path,
  spec: SessionSpec,
  cancel: CancellationToken,
) -> Result<SessionOutcome> {
  let profile_yaml = session_profile_yaml(spec.driver, &spec.user);
  let session_profile = needy_config::profile::load(&needy_config::Source::text(profile_yaml.clone(), PathBuf::new()))?;
  let driver = driver::new(&actor::host(daemon), &session_profile.actor)?;
  let channels = driver.channels(dir);
  let guest_bin = actor::guest_needy(&daemon.config)?;

  let session_cancel = cancel.child_token();
  let tasks = TaskTracker::new();

  let guest_config = build_guest_config(session_id, &channels, spec.clone_parts());
  let upper_setting = guest_config.session.upper;
  let guest_service = Arc::new(SessionGuest {
    config: guest_config,
    dir: dir.to_path_buf(),
    trace: spec.trace.clone(),
    exit: Mutex::new(None),
    cancel: session_cancel.clone(),
  });

  // Serve guest API on `cu.sock`.
  let health = Health {
    instance_id: uuid::Uuid::new_v4().to_string(),
    role: "control",
  };
  let caller = Caller::Actor {
    run: session_id.to_string(),
  };
  let guest_listener = Server::new(health, caller).guest(guest_service.clone()).bind(&[Listen::Unix(dir.join("cu.sock"))]).await?;
  let stop_guest = session_cancel.clone();
  let id_for_log = session_id.to_string();
  tasks.spawn(async move {
    if let Err(e) = guest_listener.serve(stop_guest).await {
      tracing::error!("session {id_for_log}: guest listener: {e:#}");
    }
  });

  // Serve gateway on `cg.sock`.
  serve_session_gateway(dir, spec.network, spec.trace, session_cancel.clone(), &tasks)?;

  let layer_disks = spec.layer_disks.into_iter().map(|d| d.path).collect();
  let mut vm = match daemon
    .holder
    .start(crate::holder::Actor {
      run: session_id.to_string(),
      dir: dir.to_path_buf(),
      host: actor::host(daemon),
      profile_yaml,
      recipe: None,
      mounts: Vec::new(),
      guest: guest_bin,
      layer_disks,
    })
    .await
  {
    Ok(vm) => vm,
    Err(e) => {
      session_cancel.cancel();
      tasks.close();
      tasks.wait().await;
      return Err(e);
    }
  };

  let waited = tokio::select! {
    waited = vm.wait() => waited,
    () = cancel.cancelled() => {
      // Stop the session VM cleanly on cancellation; errors are logged only.
      if let Err(e) = vm.stop(SESSION_STOP_GRACE).await {
        tracing::warn!("session {session_id}: cannot stop VM {}: {e}", vm.pid());
      }
      session_cancel.cancel();
      tasks.close();
      tasks.wait().await;
      bail!("image session {session_id} was cancelled");
    }
  };

  session_cancel.cancel();
  tasks.close();
  tasks.wait().await;

  let console = std::fs::read_to_string(driver::log(dir)).unwrap_or_default();
  if let Err(e) = waited {
    bail!("waiting for image session {session_id} failed: {e}");
  }

  let exit = guest_service.exit();
  let succeeded = matches!(&exit, Some(Exit { code: Some(0), signal }) if signal.is_empty());
  let layer = if upper_setting == UpperLayer::Kept && succeeded {
    let upper = dir.join(UPPER_LAYER);
    if upper.exists() {
      let store = daemon.images.store.clone();
      let digest = tokio::task::spawn_blocking(move || -> Result<LayerDigest> {
        let file = std::fs::File::open(&upper).with_context(|| format!("cannot open {}", upper.display()))?;
        Ok(store.put(file)?)
      })
      .await
      .context("storing built layer panicked")??;
      Some(digest)
    } else {
      None
    }
  } else {
    None
  };

  Ok(SessionOutcome {
    exit,
    layer,
    console,
  })
}

/// Resolved plan, built/cached layers (including any commit layer), and
/// materialized snapshot disks.
struct ResolvedImage {
  plan: Plan,
  snapshot: Snapshot,
  layer_disks: Vec<LayerDisk>,
  steps: Vec<BuildStepOutcome>,
}

/// Plans `definition` over the daemon's base VM image, builds any missing
/// `run` steps under single-flight deduplication, stacks `commit` on top if
/// present, and materializes the snapshot's SquashFS disks.
async fn build_image_plan(
  daemon: &Daemon,
  definition: &Definition,
  driver: ActorDriver,
  user: &User,
  commit: Option<&PlanCommit>,
  trace: &Dispatch,
  cancel: &CancellationToken,
) -> Result<ResolvedImage> {
  let root = root_digest(daemon).await?;
  let plan = needy_core::image::plan(definition, &root, user, commit);
  let mut layers = Vec::with_capacity(plan.steps.len() + usize::from(plan.commit.is_some()));
  let mut steps = Vec::with_capacity(plan.steps.len());

  for step in &plan.steps {
    if cancel.is_cancelled() {
      bail!("the daemon shut down while building the image");
    }
    let (layer, cached, ms) = daemon.images.resolve_step(daemon, driver, trace, cancel, &layers, step).await?;
    let Input::Run(script) = &step.input;
    let event = Event::ImageBuildStep(ImageBuildStep {
      key: step.key.to_string(),
      layer: layer.to_string(),
      script: script.clone(),
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
    let commit_layer: LayerDigest =
      commit.layer.to_string().parse().with_context(|| format!("invalid commit layer digest {}", commit.layer))?;
    if !daemon.images.store.has_layer(&commit_layer) {
      bail!(
        "commit layer {} from the image lockfile is not in this daemon's layer store; roll it back or remove it from the lockfile",
        commit.layer
      );
    }
    layers.push(commit_layer);
  }

  let store = daemon.images.store.clone();
  let (snapshot, layer_disks) = tokio::task::spawn_blocking(move || -> Result<_> {
    let snapshot = store.snapshot(&layers)?;
    let disks = store.materialize(&snapshot)?;
    Ok((snapshot, disks))
  })
  .await
  .context("materializing snapshot panicked")??;

  Ok(ResolvedImage {
    plan,
    snapshot,
    layer_disks,
    steps,
  })
}

/// Resolves `run`'s `actor.image` when it is an [`Image::Definition`]: builds
/// any missing `run` layers, stacks any lockfile commit on top, materializes
/// the resulting snapshot, and installs it on `run` via [`Run::set_boot`].
/// No-op when `actor.image` is absent or an OCI image reference.
// TODO(images): auto-pulling a missing base VM image on `run create` comes
// with slice 3 of the images and run data spec ("Slices"); until then
// `actor::check_host` fails before this runs with the `needy image pull`
// advice.
pub(crate) async fn prepare(daemon: &Daemon, run: &Run) -> Result<()> {
  let Some(Image::Definition(definition)) = &run.profile.actor.image else {
    return Ok(());
  };
  let resolved =
    build_image_plan(daemon, definition, run.profile.actor.driver, &run.profile.user, run.image_commit.as_ref(), &run.trace, &run.cancel)
      .await?;

  let commit_rebased = resolved.plan.commit_rebased();
  run.set_boot(Boot {
    snapshot: Some(resolved.snapshot),
    layer_disks: resolved.layer_disks,
    path: resolved.plan.path,
    env: resolved.plan.env,
    session: GuestSession::default(),
    commit_rebased,
  });
  Ok(())
}

#[async_trait::async_trait]
impl ImagesTrait for Daemon {
  async fn build(&self, request: ImageBuild) -> Result<ImageBuildResult, ControlError> {
    self.admit_work()?;
    let definition = parse_definition(&request.image_yaml)?;
    let driver = parse_driver(&request.driver)?;
    let user = request.user.unwrap_or_default();
    check_driver_host(self, driver).await?;

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
      build_image_plan(self, &definition, driver, &user, request.commit.as_ref(), &trace, &self.cancel).await.map_err(crate::failed)?;

    Ok(ImageBuildResult {
      snapshot: resolved.snapshot.digest.to_string(),
      commit_rebased: resolved.plan.commit_rebased(),
      steps: resolved.steps,
    })
  }

  async fn start_edit(&self, request: ImageEditStart) -> Result<ImageEditStarted, ControlError> {
    self.admit_work()?;
    let definition = parse_definition(&request.image_yaml)?;
    let driver = parse_driver(&request.driver)?;
    let user = request.user.unwrap_or_default();
    check_driver_host(self, driver).await?;

    // Enforce one edit session per image at a time. The guard stays alive
    // inside the background watcher task until the edit session ends.
    let lock_guard = self.images.lock_edit(&request.image_name)?;

    let session_id = format!("e-{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
    let trace = Dispatch::new(
      &session_id,
      Vec::new(),
      needy_trace::Options {
        capacity: 256,
        version: needy_core::event::SCHEMA_VERSION,
        timing: false,
      },
    );

    let resolved =
      build_image_plan(self, &definition, driver, &user, request.commit.as_ref(), &trace, &self.cancel).await.map_err(crate::failed)?;

    let base_top = resolved.plan.top().clone();
    let commit_rebased = resolved.plan.commit_rebased();
    let parent_layer: Option<LayerDigest> = match &request.commit {
      Some(c) => Some(c.layer.to_string().parse().map_err(|e| ControlError::Invalid(format!("invalid commit layer {}: {e}", c.layer)))?),
      None => None,
    };

    let dir = self.config.state_dir.join("sessions").join(&session_id);
    needy_api_server::create_private_dir(&dir).with_context(|| format!("cannot create {}", dir.display())).map_err(crate::failed)?;

    let root_user = User {
      name: "root".into(),
      uid: 0,
      gid: 0,
    };
    let profile_yaml = session_profile_yaml(driver, &root_user);
    let session_profile =
      needy_config::profile::load(&needy_config::Source::text(profile_yaml.clone(), PathBuf::new())).map_err(|e| crate::failed(e.into()))?;
    let actor_driver = driver::new(&actor::host(self), &session_profile.actor).map_err(|e| crate::failed(e.into()))?;
    let channels = actor_driver.channels(&dir);
    let guest_bin = actor::guest_needy(&self.config).map_err(crate::failed)?;

    let session_cancel = self.cancel.child_token();
    let tasks = TaskTracker::new();
    let commands: Arc<Mutex<Vec<StoreCommand>>> = Arc::default();

    // Spec ("Recording an edit session"): route every exec (`*`) and decide
    // every one as local (`Target::Actor`) so each program is traced and
    // recorded with the commit.
    let guest_config = GuestConfig {
      run: session_id.clone(),
      user: GuestUser {
        name: root_user.name.clone(),
        uid: root_user.uid,
        gid: root_user.gid,
      },
      route_names: vec!["*".into()],
      hostname: format!("needy-{session_id}"),
      uptime_offset_secs: 0,
      mounts: Vec::new(),
      command: vec!["/bin/sh".into()],
      env: resolved.plan.env.into_iter().collect(),
      cwd: "/".into(),
      gateway_port: channels.gateway_port,
      sensors: Vec::new(),
      copy_in: Vec::new(),
      mirror_out: Vec::new(),
      sensor_faults: Vec::new(),
      tty: Some(request.tty.unwrap_or(WindowSize { rows: 24, cols: 80 })),
      process_port: channels.process_port,
      wait_for_attach: request.wait_for_attach,
      daemon_uid: daemon_uid(),
      daemon_gid: daemon_gid(),
      layers: resolved.snapshot.layers.iter().map(ToString::to_string).collect(),
      image_path: resolved.plan.path,
      session: GuestSession {
        upper: UpperLayer::Kept,
        routing: true,
      },
    };

    let guest_service = Arc::new(SessionGuest {
      config: guest_config,
      dir: dir.clone(),
      trace: trace.clone(),
      exit: Mutex::new(None),
      cancel: session_cancel.clone(),
    });
    let runs_service = Arc::new(EditSessionRuns {
      commands: commands.clone(),
      trace: trace.clone(),
    });

    // Serve GuestService AND RunService (for Decide) on `cu.sock`.
    let health = Health {
      instance_id: uuid::Uuid::new_v4().to_string(),
      role: "control",
    };
    let caller = Caller::Actor {
      run: session_id.clone(),
    };
    let guest_listener = Server::new(health, caller)
      .guest(guest_service.clone())
      .runs(runs_service)
      .bind(&[Listen::Unix(dir.join("cu.sock"))])
      .await
      .map_err(|e| crate::failed(e.into()))?;
    let stop_guest = session_cancel.clone();
    let id_for_log = session_id.clone();
    tasks.spawn(async move {
      if let Err(e) = guest_listener.serve(stop_guest).await {
        tracing::error!("edit session {id_for_log}: guest listener: {e:#}");
      }
    });

    let network = Policy {
      allow: vec![Glob::new("*")],
      services: BTreeMap::new(),
    };
    if let Err(e) = serve_session_gateway(&dir, network, trace, session_cancel.clone(), &tasks) {
      session_cancel.cancel();
      tasks.close();
      tasks.wait().await;
      cleanup_session_dir(&dir);
      return Err(crate::failed(e));
    }

    let layer_disks = resolved.layer_disks.into_iter().map(|d| d.path).collect();
    let mut vm = match self
      .holder
      .start(crate::holder::Actor {
        run: session_id.clone(),
        dir: dir.clone(),
        host: actor::host(self),
        profile_yaml,
        recipe: None,
        mounts: Vec::new(),
        guest: guest_bin,
        layer_disks,
      })
      .await
    {
      Ok(vm) => vm,
      Err(e) => {
        session_cancel.cancel();
        tasks.close();
        tasks.wait().await;
        cleanup_session_dir(&dir);
        return Err(crate::failed(e));
      }
    };

    let (result_tx, result_rx) = watch::channel(None);
    let aborted = Arc::new(AtomicBool::new(false));
    let session = Arc::new(EditSession {
      id: session_id.clone(),
      channels,
      cancel: session_cancel.clone(),
      aborted: aborted.clone(),
      terminal: tokio::sync::OnceCell::new(),
      result_rx,
    });
    self.images.edits.lock().unwrap_or_else(|p| p.into_inner()).insert(session_id.clone(), session);

    let store = self.images.store.clone();
    let image_name = request.image_name;
    let id_for_task = session_id.clone();
    self.tasks.spawn(async move {
      let _lock_guard = lock_guard;
      let waited = tokio::select! {
        waited = vm.wait() => Some(waited),
        () = session_cancel.cancelled() => {
          if let Err(e) = vm.stop(SESSION_STOP_GRACE).await {
            tracing::warn!("edit session {id_for_task}: cannot stop VM {}: {e}", vm.pid());
          }
          None
        }
      };

      session_cancel.cancel();
      tasks.close();
      tasks.wait().await;

      let recorded: Vec<StoreCommand> = commands.lock().unwrap_or_else(|p| p.into_inner()).clone();
      let recorded_out: Vec<RecordedCommand> = recorded
        .iter()
        .map(|c| RecordedCommand {
          argv: c.argv.clone(),
          cwd: c.cwd.clone(),
        })
        .collect();

      let outcome = if aborted.load(Ordering::SeqCst) {
        Ok(ImageEditFinished {
          commit: None,
          commands: recorded_out,
          exit: None,
        })
      } else if let Some(Err(e)) = waited {
        Err(format!("waiting for edit session {id_for_task} failed: {e}"))
      } else {
        let exit = guest_service.exit();
        let succeeded = matches!(&exit, Some(Exit { code: Some(0), signal }) if signal.is_empty());
        let upper = dir.join(UPPER_LAYER);
        if succeeded && upper.exists() {
          let committed = tokio::task::spawn_blocking(move || -> Result<LayerDigest> {
            let file = std::fs::File::open(&upper).with_context(|| format!("cannot open {}", upper.display()))?;
            Ok(store.commit(&image_name, parent_layer.as_ref(), file, recorded)?)
          })
          .await;
          match committed {
            Ok(Ok(layer)) => match PlanDigest::parse(&layer.to_string()) {
              Ok(layer) => Ok(ImageEditFinished {
                commit: Some(PlanCommit {
                  layer,
                  base: base_top,
                }),
                commands: recorded_out,
                exit,
              }),
              Err(e) => Err(format!("committed layer digest invalid: {e}")),
            },
            Ok(Err(e)) => Err(format!("committing edit session {id_for_task}: {e:#}")),
            Err(e) => Err(format!("committing edit session {id_for_task} panicked: {e}")),
          }
        } else {
          Ok(ImageEditFinished {
            commit: None,
            commands: recorded_out,
            exit,
          })
        }
      };

      cleanup_session_dir(&dir);
      let _ = result_tx.send(Some(outcome));
    });

    Ok(ImageEditStarted {
      session_id,
      commit_rebased,
    })
  }

  async fn finish_edit(&self, session_id: &str, abort: bool) -> Result<ImageEditFinished, ControlError> {
    let session = self
      .images
      .edits
      .lock()
      .unwrap_or_else(|p| p.into_inner())
      .remove(session_id)
      .ok_or_else(|| ControlError::NotFound(format!("no active edit session {session_id}")))?;
    if abort {
      session.aborted.store(true, Ordering::SeqCst);
      session.cancel.cancel();
    }
    let mut rx = session.result_rx.clone();
    let result = rx
      .wait_for(Option::is_some)
      .await
      .map_err(|_| ControlError::Internal(format!("edit session {session_id} ended without a result")))?
      .clone()
      .expect("wait_for guarantees Some");
    result.map_err(ControlError::Failed)
  }

  async fn rollback(&self, request: ImageRollback) -> Result<ImageRollbackResult, ControlError> {
    self.admit_work()?;
    if self.images.active_edits.lock().unwrap_or_else(|p| p.into_inner()).contains(&request.image_name) {
      return Err(ControlError::Failed(format!("cannot roll back image {}: an edit session is running", request.image_name)));
    }
    let from: LayerDigest =
      request.from_layer.parse().map_err(|e| ControlError::Invalid(format!("invalid commit layer {}: {e}", request.from_layer)))?;
    let store = self.images.store.clone();
    let image_name = request.image_name;
    let steps = request.steps as usize;
    let target = tokio::task::spawn_blocking(move || store.rollback(&image_name, &from, steps))
      .await
      .map_err(|e| ControlError::Internal(format!("rollback panicked: {e}")))?
      .map_err(|e| match e {
        StoreError::UnknownCommit { .. } | StoreError::Missing(_) => ControlError::NotFound(e.to_string()),
        StoreError::TooFar { .. } | StoreError::Invalid(_) => ControlError::Invalid(e.to_string()),
        StoreError::Io { .. } => ControlError::Failed(e.to_string()),
      })?;
    Ok(ImageRollbackResult {
      layer: target.map(|d| d.to_string()),
    })
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

async fn check_driver_host(daemon: &Daemon, driver: ActorDriver) -> Result<(), ControlError> {
  let profile_yaml = session_profile_yaml(driver, &User::default());
  let profile =
    needy_config::profile::load(&needy_config::Source::text(profile_yaml, PathBuf::new())).map_err(|e| crate::failed(e.into()))?;
  let drv = driver::new(&actor::host(daemon), &profile.actor).map_err(|e| crate::failed(e.into()))?;
  drv.check_host().await.map_err(|e| crate::failed(e.into()))?;
  actor::guest_needy(&daemon.config).map_err(crate::failed)?;
  Ok(())
}

fn session_profile_yaml(driver: ActorDriver, user: &User) -> String {
  format!("actor:\n  driver: {}\nuser:\n  name: {}\n  uid: {}\n  gid: {}\n", driver.as_str(), user.name, user.uid, user.gid,)
}

fn serve_session_gateway(dir: &Path, network: Policy, trace: Dispatch, cancel: CancellationToken, tasks: &TaskTracker) -> Result<()> {
  let gw_path = dir.join("cg.sock");
  let gw = Arc::new(gateway::Gateway::new(network, trace, Network::Actor, None, cancel));
  let gw_listener = tokio::net::UnixListener::bind(&gw_path).with_context(|| format!("cannot listen on {}", gw_path.display()))?;
  {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&gw_path, std::fs::Permissions::from_mode(0o600))
      .with_context(|| format!("cannot restrict {}", gw_path.display()))?;
  }
  tasks.spawn(gw.run(tokio_stream::wrappers::UnixListenerStream::new(gw_listener)));
  Ok(())
}

struct GuestConfigParts {
  snapshot: Option<Snapshot>,
  image_path: String,
  user: User,
  command: Vec<String>,
  cwd: String,
  env: BTreeMap<String, String>,
  route_names: Vec<String>,
  session: GuestSession,
  tty: Option<WindowSize>,
  wait_for_attach: bool,
}

impl SessionSpec {
  fn clone_parts(&self) -> GuestConfigParts {
    GuestConfigParts {
      snapshot: self.snapshot.clone(),
      image_path: self.image_path.clone(),
      user: self.user.clone(),
      command: self.command.clone(),
      cwd: self.cwd.clone(),
      env: self.env.clone(),
      route_names: self.route_names.clone(),
      session: self.session,
      tty: self.tty,
      wait_for_attach: self.wait_for_attach,
    }
  }
}

fn build_guest_config(session_id: &str, channels: &driver::Channels, parts: GuestConfigParts) -> GuestConfig {
  GuestConfig {
    run: session_id.to_string(),
    user: GuestUser {
      name: parts.user.name,
      uid: parts.user.uid,
      gid: parts.user.gid,
    },
    route_names: parts.route_names,
    hostname: format!("needy-{session_id}"),
    uptime_offset_secs: 0,
    mounts: Vec::new(),
    command: parts.command,
    env: parts.env.into_iter().collect(),
    cwd: parts.cwd,
    gateway_port: channels.gateway_port,
    sensors: Vec::new(),
    copy_in: Vec::new(),
    mirror_out: Vec::new(),
    sensor_faults: Vec::new(),
    tty: parts.tty,
    process_port: channels.process_port,
    wait_for_attach: parts.wait_for_attach,
    daemon_uid: daemon_uid(),
    daemon_gid: daemon_gid(),
    layers: parts.snapshot.as_ref().map(|s| s.layers.iter().map(ToString::to_string).collect()).unwrap_or_default(),
    image_path: parts.image_path,
    session: parts.session,
  }
}

/// Computes the root digest for the base VM image configured on `daemon`.
///
/// NOTICE: in slice 1, image definitions on VM drivers boot the host's base VM
/// image (`daemon.config.actor_image`) with built layers stacked over it. The
/// root digest is taken from `manifest.json`'s `sha256` field when present
/// (released images), or computed as the SHA-256 of `rootfs.ext4` when absent
/// (locally built images and test fixtures).
// TODO(images): resolving an OCI `definition.root` to its own lower layers
// comes with slice 5 of the images and run data spec.
pub(crate) async fn root_digest(daemon: &Daemon) -> Result<PlanDigest> {
  let dir = daemon.config.actor_image.clone();
  let images = daemon.images.clone();
  tokio::task::spawn_blocking(move || -> Result<PlanDigest> {
    if let Some(manifest) = manifest::read(&dir)?
      && let Some(sha256) = manifest.sha256
      && let Ok(digest) = PlanDigest::parse(&format!("sha256:{sha256}"))
    {
      return Ok(digest);
    }
    let rootfs = dir.join("rootfs.ext4");
    let meta = std::fs::metadata(&rootfs).with_context(|| format!("cannot stat {} to hash base rootfs", rootfs.display()))?;
    let len = meta.len();
    let modified = meta.modified().ok();
    if let Some(cached) = images.root_digest_cache.lock().unwrap_or_else(|p| p.into_inner()).as_ref()
      && cached.path == rootfs
      && cached.len == len
      && cached.modified == modified
    {
      return Ok(cached.digest.clone());
    }
    let mut file = std::fs::File::open(&rootfs).with_context(|| format!("cannot open {} to hash base rootfs", rootfs.display()))?;
    let mut hasher = sha2::Sha256::new();
    std::io::copy(&mut file, &mut hasher).with_context(|| format!("cannot hash {}", rootfs.display()))?;
    let text = format!("sha256:{:x}", hasher.finalize());
    let digest = PlanDigest::parse(&text)?;
    *images.root_digest_cache.lock().unwrap_or_else(|p| p.into_inner()) = Some(CachedRootDigest {
      path: rootfs,
      len,
      modified,
      digest: digest.clone(),
    });
    Ok(digest)
  })
  .await
  .context("hashing base VM image panicked")?
}

/// What `GuestService` serves to an image session's guest init.
struct SessionGuest {
  config: GuestConfig,
  dir: PathBuf,
  trace: Dispatch,
  exit: Mutex<Option<Exit>>,
  cancel: CancellationToken,
}

impl SessionGuest {
  fn exit(&self) -> Option<Exit> {
    self.exit.lock().unwrap_or_else(|p| p.into_inner()).clone()
  }
}

#[async_trait::async_trait]
impl Guest for SessionGuest {
  async fn config(&self, _run: &str) -> Result<GuestConfig, ControlError> {
    Ok(self.config.clone())
  }

  async fn event(&self, _run: &str, time_ms: i64, event: Event) -> Result<(), ControlError> {
    self.trace.emit_at(time_ms, event.kind().as_str(), event.raw());
    Ok(())
  }

  async fn exit(&self, _run: &str, exit: Exit) -> Result<(), ControlError> {
    *self.exit.lock().unwrap_or_else(|p| p.into_inner()) = Some(exit);
    Ok(())
  }

  async fn fetch_files(&self, _run: &str, _name: &str) -> Result<Vec<u8>, ControlError> {
    Err(ControlError::Failed("file transfers are not supported in image sessions".into()))
  }

  async fn push_files(&self, _run: &str, _name: &str, _archive: Vec<u8>) -> Result<(), ControlError> {
    Err(ControlError::Failed("file transfers are not supported in image sessions".into()))
  }

  async fn push_layer(&self, run: &str, archive: Chunks) -> Result<(), ControlError> {
    if self.config.session.upper != UpperLayer::Kept {
      return Err(ControlError::Failed(format!("session {run} keeps no upper layer")));
    }
    keep_upper(&self.dir.join(UPPER_LAYER), archive)
      .await
      .map_err(|e| ControlError::Failed(format!("session {run}: keeping the upper layer: {e:#}")))
  }

  async fn control(&self, _run: &str) -> Result<GuestControls, ControlError> {
    let cancel = self.cancel.clone();
    let stream = futures_util::stream::unfold(cancel, |cancel| async move {
      cancel.cancelled().await;
      None
    });
    Ok(Box::pin(stream))
  }
}

/// What `RunService` serves to an interactive edit session's guest proxy:
/// decides every routed exec as local (`Target::Actor`) so it runs inside the
/// actor and records it for the commit history and `run:` snippet.
struct EditSessionRuns {
  commands: Arc<Mutex<Vec<StoreCommand>>>,
  trace: Dispatch,
}

#[async_trait::async_trait]
impl Runs for EditSessionRuns {
  async fn decide(&self, _run: &str, ctx: ExecContext) -> Result<Route, ControlError> {
    self.commands.lock().unwrap_or_else(|p| p.into_inner()).push(StoreCommand::new(ctx.argv.clone(), ctx.cwd.clone()));
    let event = Event::ExecDecide(ExecDecide {
      argv: ctx.argv.clone(),
      effective: ctx.argv.clone(),
      origin: Origin::ActorProxy,
      outcome: Outcome::Run(Placement::Actor),
      rules: Vec::new(),
    });
    self.trace.emit(event.kind().as_str(), event.raw());
    Ok(Route {
      target: Target::Actor,
      argv: ctx.argv,
      env: HashMap::new(),
      clear_env: false,
      cwd: ctx.cwd,
      search_path_if_missing: false,
    })
  }

  async fn create(&self, _request: NewRun) -> Result<RunInfo, ControlError> {
    Err(ControlError::role_denied("edit session actors cannot create runs"))
  }
  async fn get(&self, _id: &str) -> Result<RunInfo, ControlError> {
    Err(ControlError::role_denied("edit session actors cannot read runs"))
  }
  async fn list(&self) -> Result<Vec<RunInfo>, ControlError> {
    Err(ControlError::role_denied("edit session actors cannot list runs"))
  }
  async fn stop(&self, _id: &str, _by: Principal, _mode: StopMode) -> Result<Option<ActorStop>, ControlError> {
    Err(ControlError::role_denied("edit session actors cannot stop runs"))
  }
  async fn logs(&self, _id: &str, _follow: bool) -> Result<Logs, ControlError> {
    Err(ControlError::role_denied("edit session actors cannot read logs"))
  }
  async fn events(&self, _run: &str) -> Result<RunEvents, ControlError> {
    Err(ControlError::role_denied("edit session actors cannot read events"))
  }
  async fn check(&self, _profile: ProfileToCheck) -> Result<ProfileCheck, ControlError> {
    Err(ControlError::role_denied("edit session actors cannot check profiles"))
  }
  async fn add_fault(&self, _run: &str, _name: &str, _fault_yaml: &str, _by: Principal) -> Result<FaultAdded, ControlError> {
    Err(ControlError::role_denied("edit session actors cannot change chaos"))
  }
  async fn faults(&self, _run: &str) -> Result<RunFaults, ControlError> {
    Err(ControlError::role_denied("edit session actors cannot read chaos"))
  }
  async fn remove_fault(&self, _run: &str, _name: &str, _by: Principal) -> Result<String, ControlError> {
    Err(ControlError::role_denied("edit session actors cannot change chaos"))
  }
}

fn daemon_uid() -> u32 {
  // SAFETY: `geteuid` cannot fail.
  unsafe { libc::geteuid() }
}

fn daemon_gid() -> u32 {
  // SAFETY: `getegid` cannot fail.
  unsafe { libc::getegid() }
}

fn cleanup_session_dir(dir: &Path) {
  if let Err(e) = std::fs::remove_dir_all(dir)
    && e.kind() != std::io::ErrorKind::NotFound
  {
    tracing::warn!("cannot remove session directory {}: {e}", dir.display());
  }
}

fn first_line(script: &str) -> &str {
  script.lines().find(|line| !line.trim().is_empty()).unwrap_or(script).trim()
}

fn format_build_failure(script: &str, status: &str, console: &str) -> String {
  let mut msg = format!("image build step `{}` {status}", first_line(script));
  let lines: Vec<&str> = console.lines().map(str::trim_end).filter(|line| !line.trim().is_empty()).collect();
  let tail = &lines[lines.len().saturating_sub(FAILURE_LOG_LINES)..];
  if !tail.is_empty() {
    msg.push('\n');
    msg.push_str(&tail.join("\n"));
  }
  msg
}

#[cfg(test)]
#[path = "image_test.rs"]
mod tests;
