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
use futures_util::StreamExt as _;
use needy_api_server::control::{
  BuildStepOutcome, Caller, Chunks, ControlError, FaultAdded, Guest, GuestConfig, GuestControls, GuestSession, GuestUser, ImageBuild,
  ImageBuildResult, ImageEditFinished, ImageEditStart, ImageEditStarted, ImageRollback, ImageRollbackResult, Images as ImagesTrait, Logs,
  NewRun, ProfileCheck, ProfileToCheck, RecordedCommand, Route, RunEvents, RunFaults, RunInfo, Runs, StopMode, Target, UpperLayer,
  WindowSize,
};
use needy_api_server::{Health, Listen, Server};
use needy_common::process::Exit;
use needy_config::image::{Definition, Image};
use needy_config::profile::{ActorDriver, CONTAINER_IMAGE, OciRuntime, User};
use needy_config::value::Glob;
use needy_core::event::{ActorStop, Event, ExecDecide, ImageBuildStep};
use needy_core::image::{Commit as PlanCommit, Digest, Input, Plan, Step};
use needy_core::intercept::exec::ExecContext;
use needy_core::network::Policy;
use needy_core::types::{Network, Origin, Outcome, Placement, Principal};
use needy_core_actor::driver;
use needy_core_actor::driver::linux_container::LinuxContainer;
use needy_core_actor::driver::macos_apple_container::MacosAppleContainer;
use needy_core_actor::image::manifest;
use needy_core_actor::image::store::{
  Command as StoreCommand, Error as StoreError, LayerDisk, Snapshot, Store, extract_top_layer, snapshot_tag,
};
use needy_core_actor::sidecar::gateway;
use needy_core_executor::engine::{self, Engine};
use needy_trace::Dispatch;
use sha2::Digest as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::guest::{UPPER_LAYER, keep_upper};
use crate::holder::ContainerBackends;
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
type FlightResult = std::result::Result<Digest, String>;

/// Result broadcast when an interactive edit session finishes.
type EditOutcomeResult = std::result::Result<ImageEditFinished, String>;

/// Cached SHA-256 of a locally built base `rootfs.ext4`, invalidated when the
/// file's path, length, or modification time changes.
#[derive(Debug, Clone)]
struct CachedRootDigest {
  path: PathBuf,
  len: u64,
  modified: Option<SystemTime>,
  digest: Digest,
}

/// The daemon's layer store, cache-key index, single-flight build table, and
/// active interactive edit sessions.
pub struct Images {
  /// The content-addressed layer and snapshot store.
  pub store: Store,
  /// Maps `needy_core::image::Digest` cache keys to built layer digests
  /// (`sha256-<hex>` files containing `sha256:<layer-hex>\n`).
  keys_dir: PathBuf,
  /// Cached exported base OCI image tars keyed by base image ID.
  base_oci_dir: PathBuf,
  /// Container runtime boundaries (Docker-compatible engine and Apple's `container` CLI).
  backends: ContainerBackends,
  /// In-flight builds keyed by step cache key, so concurrent `run create`s
  /// build each missing cache key once and wait for the same result.
  in_flight: Mutex<HashMap<Digest, watch::Receiver<Option<FlightResult>>>>,
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
  pub(crate) fn with_backends(cache_dir: &Path, data_dir: &Path, backends: ContainerBackends) -> Self {
    let cache = cache_dir.join("image-store");
    let data = data_dir.join("image-store");
    let keys_dir = cache.join("keys");
    let base_oci_dir = cache.join("base-oci");
    Self {
      store: Store::new(cache, data),
      keys_dir,
      base_oci_dir,
      backends,
      in_flight: Mutex::default(),
      active_edits: Mutex::default(),
      edits: Mutex::default(),
      root_digest_cache: Mutex::default(),
    }
  }

  /// Container runtime boundaries used by container actor drivers and image
  /// materialization.
  pub(crate) fn backends(&self) -> &ContainerBackends {
    &self.backends
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
    let staged = self.keys_dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let mut file =
      std::fs::OpenOptions::new().write(true).create_new(true).open(&staged).with_context(|| format!("cannot stage {}", path.display()))?;
    writeln!(file, "{layer}").with_context(|| format!("cannot write {}", path.display()))?;
    file.sync_all().with_context(|| format!("cannot sync {}", path.display()))?;
    drop(file);
    std::fs::rename(&staged, &path).with_context(|| format!("cannot persist {}", path.display()))?;
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
    trace: &Dispatch,
    cancel: &CancellationToken,
    built_below: &[Digest],
    step: &Step,
  ) -> Result<(Digest, bool, u64)> {
    if let Some(layer) = self.lookup_cached(&step.key)? {
      return Ok((layer, true, 0));
    }

    enum Role {
      Leader(watch::Sender<Option<FlightResult>>),
      Waiter(watch::Receiver<Option<FlightResult>>),
      Cached(Digest),
    }

    let role = {
      let mut in_flight = self.in_flight.lock().unwrap_or_else(|p| p.into_inner());
      if let Some(layer) = self.lookup_cached(&step.key)? {
        Role::Cached(layer)
      } else if let Some(rx) = in_flight.get(&step.key) {
        Role::Waiter(rx.clone())
      } else {
        let (tx, rx) = watch::channel(None);
        in_flight.insert(step.key, rx);
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
          key: Digest,
        }
        impl Drop for Guard {
          fn drop(&mut self) {
            self.images.in_flight.lock().unwrap_or_else(|p| p.into_inner()).remove(&self.key);
          }
        }
        let _guard = Guard {
          images: self.clone(),
          key: step.key,
        };

        let started = Instant::now();
        let built = self.build_step(daemon, builder, base_container_image, trace, cancel, built_below, step).await;
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

  /// Runs a single build step in a fresh VM or container session on top of
  /// `built_below`.
  #[allow(clippy::too_many_arguments)]
  async fn build_step(
    &self,
    daemon: &Daemon,
    builder: ActorDriver,
    base_container_image: &str,
    trace: &Dispatch,
    cancel: &CancellationToken,
    built_below: &[Digest],
    step: &Step,
  ) -> Result<Digest> {
    let Input::Run(script) = &step.input;
    let (snapshot, layer_disks, snapshot_image) = if built_below.is_empty() {
      (None, Vec::new(), None)
    } else if builder == ActorDriver::LinuxContainer {
      let store = self.store.clone();
      let layers = built_below.to_vec();
      let snapshot = tokio::task::spawn_blocking(move || store.snapshot(&layers)).await.context("snapshotting parent layers panicked")??;
      let tag = materialize_for_linux_container(daemon, &snapshot, base_container_image).await?;
      (Some(snapshot), Vec::new(), Some(tag))
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
      (Some(snapshot), disks, None)
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
  /// Which driver to boot (`linux-firecracker`, `macos-vz`, or `linux-container`).
  pub driver: ActorDriver,
  /// Base container image reference when `driver == ActorDriver::LinuxContainer`.
  pub base_container_image: Option<String>,
  /// The layers stacked below the session's upper layer, lowest first.
  pub snapshot: Option<Snapshot>,
  /// Materialized SquashFS layer disks of `snapshot`, lowest first (VM drivers).
  pub layer_disks: Vec<LayerDisk>,
  /// Materialized OCI snapshot tag (container driver).
  pub snapshot_image: Option<String>,
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
  pub layer: Option<Digest>,
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
  if spec.driver == ActorDriver::LinuxContainer {
    return run_container_session_in(daemon, session_id, dir, spec, cancel).await;
  }
  let profile_yaml = session_profile_yaml(spec.driver, &spec.user, None);
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
  serve_session_gateway(&dir.join("cg.sock"), spec.network, spec.trace, session_cancel.clone(), &tasks, false)?;

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
      snapshot_image: None,
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
      let digest = tokio::task::spawn_blocking(move || -> Result<Digest> {
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

/// Runs a build step in a writable `linux-container` session (used on Linux
/// hosts without a VM driver), committing the stopped container's upper layer
/// into the layer store on clean exit.
async fn run_container_session_in(
  daemon: &Daemon,
  session_id: &str,
  dir: &Path,
  spec: SessionSpec,
  cancel: CancellationToken,
) -> Result<SessionOutcome> {
  let engine = daemon.images.backends().engine.clone();
  let base_image = spec.base_container_image.as_deref().unwrap_or(CONTAINER_IMAGE).to_string();
  let container_driver = LinuxContainer::with_engine(OciRuntime::Runc, base_image, engine.clone());
  let channels = driver::Driver::channels(&container_driver, dir);
  let guest_bin = actor::guest_needy(&daemon.config)?;

  let session_cancel = cancel.child_token();
  let tasks = TaskTracker::new();

  let mut guest_config = build_guest_config(session_id, &channels, spec.clone_parts());
  // Container sessions boot an assembled OCI image, not VM layer disks.
  guest_config.layers.clear();
  let upper_setting = guest_config.session.upper;
  let guest_service = Arc::new(SessionGuest {
    config: guest_config,
    dir: dir.to_path_buf(),
    trace: spec.trace.clone(),
    exit: Mutex::new(None),
    cancel: session_cancel.clone(),
  });

  let driver::Endpoint::Unix(guest_sock) = &channels.guest;
  let driver::Endpoint::Unix(gateway_sock) = &channels.gateway;
  if let Some(parent) = guest_sock.parent() {
    needy_api_server::create_private_dir(parent).with_context(|| format!("cannot create {}", parent.display()))?;
  }

  let health = Health {
    instance_id: uuid::Uuid::new_v4().to_string(),
    role: "control",
  };
  let caller = Caller::Actor {
    run: session_id.to_string(),
  };
  let guest_listener = Server::new(health, caller).guest(guest_service.clone()).bind(&[Listen::Unix(guest_sock.clone())]).await?;
  open_socket_permissions(guest_sock)?;
  let stop_guest = session_cancel.clone();
  let id_for_log = session_id.to_string();
  tasks.spawn(async move {
    if let Err(e) = guest_listener.serve(stop_guest).await {
      tracing::error!("session {id_for_log}: guest listener: {e:#}");
    }
  });

  serve_session_gateway(gateway_sock, spec.network, spec.trace, session_cancel.clone(), &tasks, true)?;

  let start_req = driver::Start {
    run: session_id,
    dir,
    recipe: None,
    user: &spec.user,
    mounts: &[],
    guest: &guest_bin,
    layer_disks: &[],
    snapshot_image: spec.snapshot_image.as_deref(),
  };
  let mut container = match container_driver.start_session(&start_req).await {
    Ok(container) => container,
    Err(e) => {
      session_cancel.cancel();
      tasks.close();
      tasks.wait().await;
      return Err(e.into());
    }
  };

  let waited = tokio::select! {
    waited = container.wait() => waited,
    () = cancel.cancelled() => {
      if let Err(e) = container.stop(SESSION_STOP_GRACE).await {
        tracing::warn!("session {session_id}: cannot stop container {}: {e}", container.pid());
      }
      container.remove().await;
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
    container.remove().await;
    bail!("waiting for image session {session_id} failed: {e}");
  }

  let exit = guest_service.exit();
  let succeeded = matches!(&exit, Some(Exit { code: Some(0), signal }) if signal.is_empty());
  let layer = if upper_setting == UpperLayer::Kept && succeeded {
    let committed = commit_container_to_file(&engine, container.container_name(), dir).await;
    container.remove().await;
    let committed_tar = committed?;
    let store = daemon.images.store.clone();
    let digest = tokio::task::spawn_blocking(move || store.put_from_image_export(&committed_tar))
      .await
      .context("storing committed container layer panicked")??;
    Some(digest)
  } else {
    container.remove().await;
    None
  };

  Ok(SessionOutcome {
    exit,
    layer,
    console,
  })
}

/// Resolved plan, builder driver, built/cached layers (including any commit
/// layer), and materialized snapshot (layer disks for VM drivers, or an OCI
/// image tag for container drivers).
struct ResolvedImage {
  builder: ActorDriver,
  plan: Plan,
  snapshot: Snapshot,
  layer_disks: Vec<LayerDisk>,
  snapshot_image: Option<String>,
  steps: Vec<BuildStepOutcome>,
}

/// Plans `definition`, builds any missing `run` steps under single-flight
/// deduplication on the host's builder driver (VM driver when available, else
/// `linux-container` on Linux; never Apple's `container`), stacks `commit` on
/// top if present, and materializes the snapshot for `target_driver`.
async fn build_image_plan(
  daemon: &Daemon,
  definition: &Definition,
  target_driver: ActorDriver,
  user: &User,
  commit: Option<&PlanCommit>,
  trace: &Dispatch,
  cancel: &CancellationToken,
) -> Result<ResolvedImage> {
  let base_container_image = definition.root.as_deref().unwrap_or(CONTAINER_IMAGE);
  let builder = select_builder_driver(daemon, target_driver).await;
  let root = root_digest_for(daemon, builder, base_container_image).await?;
  let plan = needy_core::image::plan(definition, &root, user, commit);
  let mut layers = Vec::with_capacity(plan.steps.len() + usize::from(plan.commit.is_some()));
  let mut steps = Vec::with_capacity(plan.steps.len());

  for step in &plan.steps {
    if cancel.is_cancelled() {
      bail!("the daemon shut down while building the image");
    }
    let (layer, cached, ms) = daemon.images.resolve_step(daemon, builder, base_container_image, trace, cancel, &layers, step).await?;
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
    if !daemon.images.store.has_layer(&commit.layer) {
      bail!(
        "commit layer {} from the image lockfile is not in this daemon's layer store; roll it back or remove it from the lockfile",
        commit.layer
      );
    }
    layers.push(commit.layer);
  }

  let store = daemon.images.store.clone();
  let (snapshot, layer_disks, snapshot_image) = match target_driver {
    ActorDriver::LinuxFirecracker | ActorDriver::MacosVz => {
      let (snapshot, disks) = tokio::task::spawn_blocking(move || -> Result<_> {
        let snapshot = store.snapshot(&layers)?;
        let disks = store.materialize(&snapshot)?;
        Ok((snapshot, disks))
      })
      .await
      .context("materializing snapshot panicked")??;
      (snapshot, disks, None)
    }
    ActorDriver::LinuxContainer => {
      let snapshot = tokio::task::spawn_blocking(move || store.snapshot(&layers)).await.context("snapshotting layers panicked")??;
      let tag = materialize_for_linux_container(daemon, &snapshot, base_container_image).await?;
      (snapshot, Vec::new(), Some(tag))
    }
    ActorDriver::MacosAppleContainer => {
      let snapshot = tokio::task::spawn_blocking(move || store.snapshot(&layers)).await.context("snapshotting layers panicked")??;
      let tag = materialize_for_apple_container(daemon, &snapshot, base_container_image).await?;
      (snapshot, Vec::new(), Some(tag))
    }
  };

  Ok(ResolvedImage {
    builder,
    plan,
    snapshot,
    layer_disks,
    snapshot_image,
    steps,
  })
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
  let resolved =
    build_image_plan(daemon, definition, run.profile.actor.driver, &run.profile.user, run.image_commit.as_ref(), &run.trace, &run.cancel)
      .await?;

  let commit_rebased = resolved.plan.commit_rebased();
  run.set_boot(Boot {
    snapshot: Some(resolved.snapshot),
    layer_disks: resolved.layer_disks,
    snapshot_image: resolved.snapshot_image,
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
    let resolved = build_image_plan(self, &definition, target_driver, &user, request.commit.as_ref(), &trace, &self.cancel)
      .await
      .map_err(crate::failed)?;

    Ok(ImageBuildResult {
      snapshot: resolved.snapshot.digest.to_string(),
      commit_rebased: resolved.plan.commit_rebased(),
      steps: resolved.steps,
    })
  }

  async fn start_edit(&self, request: ImageEditStart) -> Result<ImageEditStarted, ControlError> {
    self.admit_work()?;
    let definition = parse_definition(&request.image_yaml)?;
    let target_driver = parse_driver(&request.driver)?;
    let user = request.user.unwrap_or_default();
    let base_container_image = definition.root.as_deref().unwrap_or(CONTAINER_IMAGE);
    let builder = select_builder_driver(self, target_driver).await;
    check_driver_host(self, builder, base_container_image).await?;

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

    // Build and materialize for the builder driver (VM layer disks, or an OCI
    // image in Docker when falling back to `linux-container` on a Linux host
    // without a VM driver).
    let resolved =
      build_image_plan(self, &definition, builder, &user, request.commit.as_ref(), &trace, &self.cancel).await.map_err(crate::failed)?;

    let base_top = *resolved.plan.top();
    let commit_rebased = resolved.plan.commit_rebased();
    let parent_layer: Option<Digest> = request.commit.as_ref().map(|c| c.layer);

    let dir = self.config.state_dir.join("sessions").join(&session_id);
    needy_api_server::create_private_dir(&dir).with_context(|| format!("cannot create {}", dir.display())).map_err(crate::failed)?;

    let root_user = User {
      name: "root".into(),
      uid: 0,
      gid: 0,
    };
    let guest_bin = actor::guest_needy(&self.config).map_err(crate::failed)?;

    let session_cancel = self.cancel.child_token();
    let tasks = TaskTracker::new();
    let commands: Arc<Mutex<Vec<StoreCommand>>> = Arc::default();

    let is_container_builder = resolved.builder == ActorDriver::LinuxContainer;
    let (channels, profile_yaml) = if is_container_builder {
      let drv = LinuxContainer::with_engine(OciRuntime::Runc, base_container_image.to_string(), self.images.backends().engine.clone());
      (driver::Driver::channels(&drv, &dir), String::new())
    } else {
      let profile_yaml = session_profile_yaml(resolved.builder, &root_user, None);
      let session_profile = needy_config::profile::load(&needy_config::Source::text(profile_yaml.clone(), PathBuf::new()))
        .map_err(|e| crate::failed(e.into()))?;
      let actor_driver = driver::new(&actor::host(self), &session_profile.actor).map_err(|e| crate::failed(e.into()))?;
      (actor_driver.channels(&dir), profile_yaml)
    };

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
      layers: if is_container_builder {
        Vec::new()
      } else {
        resolved.layer_disks.iter().map(|disk| disk.digest.to_string()).collect()
      },
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

    let (guest_sock, gateway_sock) = if is_container_builder {
      let driver::Endpoint::Unix(g) = &channels.guest;
      let driver::Endpoint::Unix(gw) = &channels.gateway;
      if let Some(parent) = g.parent() {
        needy_api_server::create_private_dir(parent)
          .with_context(|| format!("cannot create {}", parent.display()))
          .map_err(crate::failed)?;
      }
      (g.clone(), gw.clone())
    } else {
      (dir.join("cu.sock"), dir.join("cg.sock"))
    };

    // Serve GuestService AND RunService (for Decide).
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
      .bind(&[Listen::Unix(guest_sock.clone())])
      .await
      .map_err(|e| crate::failed(e.into()))?;
    if is_container_builder {
      open_socket_permissions(&guest_sock).map_err(crate::failed)?;
    }
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
    if let Err(e) = serve_session_gateway(&gateway_sock, network, trace, session_cancel.clone(), &tasks, is_container_builder) {
      session_cancel.cancel();
      tasks.close();
      tasks.wait().await;
      cleanup_session_dir(&dir);
      return Err(crate::failed(e));
    }

    enum EditRuntime {
      Vm(Box<dyn driver::Running>),
      Container {
        container: needy_core_actor::driver::linux_container::SessionContainer,
        engine: Arc<dyn Engine>,
      },
    }

    let mut runtime = if is_container_builder {
      let engine = self.images.backends().engine.clone();
      let container_driver = LinuxContainer::with_engine(OciRuntime::Runc, base_container_image.to_string(), engine.clone());
      let start_req = driver::Start {
        run: &session_id,
        dir: &dir,
        recipe: None,
        user: &root_user,
        mounts: &[],
        guest: &guest_bin,
        layer_disks: &[],
        snapshot_image: resolved.snapshot_image.as_deref(),
      };
      match container_driver.start_session(&start_req).await {
        Ok(container) => EditRuntime::Container { container, engine },
        Err(e) => {
          session_cancel.cancel();
          tasks.close();
          tasks.wait().await;
          cleanup_session_dir(&dir);
          return Err(crate::failed(e.into()));
        }
      }
    } else {
      let layer_disks = resolved.layer_disks.into_iter().map(|d| d.path).collect();
      match self
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
          snapshot_image: None,
        })
        .await
      {
        Ok(vm) => EditRuntime::Vm(vm),
        Err(e) => {
          session_cancel.cancel();
          tasks.close();
          tasks.wait().await;
          cleanup_session_dir(&dir);
          return Err(crate::failed(e));
        }
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
      let waited = match &mut runtime {
        EditRuntime::Vm(vm) => tokio::select! {
          waited = vm.wait() => Some(waited),
          () = session_cancel.cancelled() => {
            if let Err(e) = vm.stop(SESSION_STOP_GRACE).await {
              tracing::warn!("edit session {id_for_task}: cannot stop VM {}: {e}", vm.pid());
            }
            None
          }
        },
        EditRuntime::Container { container, .. } => tokio::select! {
          waited = container.wait() => Some(waited),
          () = session_cancel.cancelled() => {
            if let Err(e) = container.stop(SESSION_STOP_GRACE).await {
              tracing::warn!("edit session {id_for_task}: cannot stop container {}: {e}", container.pid());
            }
            None
          }
        },
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
        if let EditRuntime::Container { container, .. } = &runtime {
          container.remove().await;
        }
        Ok(ImageEditFinished {
          commit: None,
          commands: recorded_out,
          exit: None,
        })
      } else if let Some(Err(e)) = waited {
        if let EditRuntime::Container { container, .. } = &runtime {
          container.remove().await;
        }
        Err(format!("waiting for edit session {id_for_task} failed: {e}"))
      } else {
        let exit = guest_service.exit();
        let succeeded = matches!(&exit, Some(Exit { code: Some(0), signal }) if signal.is_empty());
        match &runtime {
          EditRuntime::Vm(_) => {
            let upper = dir.join(UPPER_LAYER);
            if succeeded && upper.exists() {
              let committed = tokio::task::spawn_blocking(move || -> Result<Digest> {
                let file = std::fs::File::open(&upper).with_context(|| format!("cannot open {}", upper.display()))?;
                Ok(store.commit(&image_name, parent_layer.as_ref(), Some(&base_top), file, recorded)?)
              })
              .await;
              finish_commit(committed, &id_for_task, base_top, recorded_out, exit)
            } else {
              Ok(ImageEditFinished {
                commit: None,
                commands: recorded_out,
                exit,
              })
            }
          }
          EditRuntime::Container { container, engine } => {
            if succeeded {
              let exported = commit_container_to_file(engine, container.container_name(), &dir).await;
              container.remove().await;
              match exported {
                Ok(committed_tar) => {
                  let committed = tokio::task::spawn_blocking(move || -> Result<Digest> {
                    let top = extract_top_layer(&committed_tar)?;
                    Ok(store.commit(&image_name, parent_layer.as_ref(), Some(&base_top), top, recorded)?)
                  })
                  .await;
                  finish_commit(committed, &id_for_task, base_top, recorded_out, exit)
                }
                Err(e) => Err(format!("committing edit session {id_for_task}: {e:#}")),
              }
            } else {
              container.remove().await;
              Ok(ImageEditFinished {
                commit: None,
                commands: recorded_out,
                exit,
              })
            }
          }
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
    let from: Digest =
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
      layer: target.as_ref().map(|c| c.layer.to_string()),
      base: target.and_then(|c| c.base).map(|d| d.to_string()),
    })
  }
}

fn finish_commit(
  committed: std::result::Result<Result<Digest>, tokio::task::JoinError>,
  session_id: &str,
  base_top: Digest,
  commands: Vec<RecordedCommand>,
  exit: Option<Exit>,
) -> EditOutcomeResult {
  match committed {
    Ok(Ok(layer)) => Ok(ImageEditFinished {
      commit: Some(PlanCommit {
        layer,
        base: base_top,
      }),
      commands,
      exit,
    }),
    Ok(Err(e)) => Err(format!("committing edit session {session_id}: {e:#}")),
    Err(e) => Err(format!("committing edit session {session_id} panicked: {e}")),
  }
}

/// Selects the driver that runs image build steps and interactive edit
/// sessions (spec: "Building and editing").
///
/// Prefers the host's VM driver (`macos-vz` on Apple silicon,
/// `linux-firecracker` on Linux with KVM). On a Linux host without a usable VM
/// driver, falls back to `linux-container`. Apple's `container` is never used
/// to build.
async fn select_builder_driver(daemon: &Daemon, target: ActorDriver) -> ActorDriver {
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
  let profile_yaml = session_profile_yaml(driver, &User::default(), None);
  let Ok(profile) = needy_config::profile::load(&needy_config::Source::text(profile_yaml, PathBuf::new())) else {
    return false;
  };
  let Ok(drv) = driver::new(&actor::host(daemon), &profile.actor) else {
    return false;
  };
  drv.check_host().await.is_ok()
}

/// Materializes `snapshot` on top of `base_image` and loads `needy-snapshot:<hex>`
/// into the Docker-compatible engine if not already present.
async fn materialize_for_linux_container(daemon: &Daemon, snapshot: &Snapshot, base_image: &str) -> Result<String> {
  let engine = daemon.images.backends().engine.clone();
  let inspected = engine
    .inspect_image(base_image)
    .await
    .with_context(|| format!("inspecting container base image {base_image}"))?
    .ok_or_else(|| anyhow!("container actor image {base_image} is not in the engine"))?;

  let base_tar = daemon.images.base_oci_dir.join(format!("{}.tar", sanitize_image_id(&inspected.id)));
  if !base_tar.exists() {
    write_stream_atomic(&base_tar, engine.export_image(base_image))
      .await
      .with_context(|| format!("exporting base container image {base_image}"))?;
  }

  let tag = snapshot_tag(&snapshot.digest);
  let store = daemon.images.store.clone();
  let snapshot_owned = snapshot.clone();
  let base_tar_owned = base_tar.clone();
  let tag_for_build = tag.clone();
  let oci_tar = tokio::task::spawn_blocking(move || store.materialize_container(&snapshot_owned, &base_tar_owned, &tag_for_build))
    .await
    .context("materializing container snapshot panicked")??;

  if engine.inspect_image(&tag).await.with_context(|| format!("inspecting snapshot image {tag}"))?.is_none() {
    let stream = stream_file(&oci_tar).await?;
    engine.load_image(stream).await.with_context(|| format!("loading snapshot image {tag} into the engine"))?;
  }
  Ok(tag)
}

/// Materializes `snapshot` on top of `base_image` and loads `needy-snapshot:<hex>`
/// into Apple's `container` image store if not already present.
async fn materialize_for_apple_container(daemon: &Daemon, snapshot: &Snapshot, base_image: &str) -> Result<String> {
  let apple = MacosAppleContainer::with_cli(base_image.to_string(), daemon.images.backends().apple_cli.clone());
  let id = apple
    .image_id(base_image)
    .await
    .with_context(|| format!("inspecting Apple container base image {base_image}"))?
    .ok_or_else(|| anyhow!("container actor image {base_image} is not in Apple's container store"))?;

  let base_tar = daemon.images.base_oci_dir.join(format!("apple-{}.tar", sanitize_image_id(&id)));
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

  let tag = snapshot_tag(&snapshot.digest);
  let store = daemon.images.store.clone();
  let snapshot_owned = snapshot.clone();
  let base_tar_owned = base_tar.clone();
  let tag_for_build = tag.clone();
  let oci_tar = tokio::task::spawn_blocking(move || store.materialize_container(&snapshot_owned, &base_tar_owned, &tag_for_build))
    .await
    .context("materializing container snapshot panicked")??;

  if apple.image_id(&tag).await.with_context(|| format!("inspecting snapshot image {tag}"))?.is_none() {
    apple.load(&oci_tar).await.with_context(|| format!("loading snapshot image {tag} into Apple's container store"))?;
  }
  Ok(tag)
}

/// Commits `container_name` to a temporary image, exports it to `dir/committed.tar`,
/// and always removes the temporary image afterwards.
async fn commit_container_to_file(engine: &Arc<dyn Engine>, container_name: &str, dir: &Path) -> Result<PathBuf> {
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

fn open_socket_permissions(path: &Path) -> Result<()> {
  use std::os::unix::fs::PermissionsExt;
  std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))
    .with_context(|| format!("cannot open permissions on {}", path.display()))?;
  if let Some(dir) = path.parent() {
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))
      .with_context(|| format!("cannot open permissions on {}", dir.display()))?;
  }
  Ok(())
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

fn session_profile_yaml(driver: ActorDriver, user: &User, container_image: Option<&str>) -> String {
  let mut yaml = format!("actor:\n  driver: {}\n", driver.as_str());
  if driver == ActorDriver::LinuxContainer {
    yaml.push_str("  container:\n    oci_runtime: runc\n");
  }
  if let Some(image) = container_image {
    yaml.push_str(&format!("  image: {image:?}\n"));
  }
  yaml.push_str(&format!("user:\n  name: {}\n  uid: {}\n  gid: {}\n", user.name, user.uid, user.gid));
  yaml
}

fn serve_session_gateway(
  gw_path: &Path,
  network: Policy,
  trace: Dispatch,
  cancel: CancellationToken,
  tasks: &TaskTracker,
  open_socket: bool,
) -> Result<()> {
  let gw = Arc::new(gateway::Gateway::new(network, trace, Network::Actor, None, cancel));
  let gw_listener = tokio::net::UnixListener::bind(gw_path).with_context(|| format!("cannot listen on {}", gw_path.display()))?;
  if open_socket {
    open_socket_permissions(gw_path)?;
  } else {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(gw_path, std::fs::Permissions::from_mode(0o600))
      .with_context(|| format!("cannot restrict {}", gw_path.display()))?;
  }
  tasks.spawn(gw.run(tokio_stream::wrappers::UnixListenerStream::new(gw_listener)));
  Ok(())
}

struct GuestConfigParts {
  layer_digests: Vec<String>,
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
    let layer_digests = if !self.layer_disks.is_empty() {
      self.layer_disks.iter().map(|disk| disk.digest.to_string()).collect()
    } else {
      self.snapshot.as_ref().map(|s| s.layers.iter().map(ToString::to_string).collect()).unwrap_or_default()
    };
    GuestConfigParts {
      layer_digests,
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
    layers: parts.layer_digests,
    image_path: parts.image_path,
    session: parts.session,
  }
}

/// Computes the root digest for `builder`: the base VM image's digest for VM
/// builders, or the base container image's ID for `linux-container` builders
/// on Linux hosts without a VM driver.
async fn root_digest_for(daemon: &Daemon, builder: ActorDriver, base_container_image: &str) -> Result<Digest> {
  if builder == ActorDriver::LinuxContainer {
    let engine = daemon.images.backends().engine.clone();
    let inspected = engine
      .inspect_image(base_container_image)
      .await
      .with_context(|| format!("inspecting container base image {base_container_image}"))?
      .ok_or_else(|| anyhow!("container actor image {base_container_image} is not in the engine"))?;
    return Ok(Digest::parse(&inspected.id).unwrap_or_else(|_| Digest::of(inspected.id.as_bytes())));
  }
  root_digest(daemon).await
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
pub(crate) async fn root_digest(daemon: &Daemon) -> Result<Digest> {
  let dir = daemon.config.actor_image.clone();
  let images = daemon.images.clone();
  tokio::task::spawn_blocking(move || -> Result<Digest> {
    if let Some(manifest) = manifest::read(&dir)?
      && let Some(sha256) = manifest.sha256
      && let Ok(digest) = Digest::parse(&format!("sha256:{sha256}"))
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
      return Ok(cached.digest);
    }
    let mut file = std::fs::File::open(&rootfs).with_context(|| format!("cannot open {} to hash base rootfs", rootfs.display()))?;
    let mut hasher = sha2::Sha256::new();
    std::io::copy(&mut file, &mut hasher).with_context(|| format!("cannot hash {}", rootfs.display()))?;
    let digest = Digest::from_bytes(hasher.finalize().into());
    *images.root_digest_cache.lock().unwrap_or_else(|p| p.into_inner()) = Some(CachedRootDigest {
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
