//! Image sessions and building: resolving an image definition's build plan,
//! running build steps in disposable VM sessions with single-flight
//! deduplication per cache key, caching built layers, and materializing the
//! resulting snapshot for `run create`.
//!
//! See `docs/ai/references/architecture/2026-11-17-images-and-run-data-spec.md`
//! ("Resolving and planning a build", "The layer store", "Building and editing").

use std::collections::{BTreeMap, HashMap};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use needy_api_server::control::{
  Caller, Chunks, ControlError, Guest, GuestConfig, GuestControls, GuestSession, GuestUser, UpperLayer, WindowSize,
};
use needy_api_server::{Health, Listen, Server};
use needy_common::process::Exit;
use needy_config::image::Image;
use needy_config::profile::{ActorDriver, User};
use needy_config::value::Glob;
use needy_core::event::{Event, ImageBuildStep};
use needy_core::image::{Digest as PlanDigest, Input, Step};
use needy_core::network::Policy;
use needy_core::types::Network;
use needy_core_actor::driver;
use needy_core_actor::image::manifest;
use needy_core_actor::image::store::{Digest as LayerDigest, LayerDisk, Snapshot, Store};
use needy_core_actor::sidecar::gateway;
use needy_trace::Dispatch;
use sha2::Digest as _;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::guest::{UPPER_LAYER, keep_upper};
use crate::run::{Boot, Run};
use crate::{Daemon, actor};

/// How long a cancelled build session's VM gets to exit on SIGTERM before
/// SIGKILL.
const SESSION_STOP_GRACE: Duration = Duration::from_secs(3);

/// How many trailing non-empty lines of a failed build session's console log
/// to include in the error message.
const FAILURE_LOG_LINES: usize = 20;

/// Result broadcast to waiters sharing an in-flight build for one cache key.
type FlightResult = std::result::Result<LayerDigest, String>;

/// The daemon's layer store, cache-key index, and single-flight build table.
pub struct Images {
  /// The content-addressed layer and snapshot store.
  pub store: Store,
  /// Maps `needy_core::image::Digest` cache keys to built layer digests
  /// (`sha256-<hex>` files containing `sha256:<layer-hex>\n`).
  keys_dir: PathBuf,
  /// In-flight builds keyed by step cache key, so concurrent `run create`s
  /// build each missing cache key once and wait for the same result.
  in_flight: Mutex<HashMap<PlanDigest, watch::Receiver<Option<FlightResult>>>>,
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
    }
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
    run: &Run,
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
          () = run.cancel.cancelled() => bail!("the daemon shut down while waiting for an image build"),
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
        let built = self.build_step(daemon, run, built_below, step).await;
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
  async fn build_step(&self, daemon: &Daemon, run: &Run, built_below: &[LayerDigest], step: &Step) -> Result<LayerDigest> {
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
      driver: run.profile.actor.driver,
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
      trace: run.trace.clone(),
    };

    let outcome = run_session(daemon, spec, run.cancel.clone()).await?;
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
/// session in slice 1 step 8).
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
///
/// Reusable across build steps (step 7) and interactive edit sessions (step 8).
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
  let profile_yaml = format!(
    "actor:\n  driver: {}\nuser:\n  name: {}\n  uid: {}\n  gid: {}\n",
    spec.driver.as_str(),
    spec.user.name,
    spec.user.uid,
    spec.user.gid,
  );
  let session_profile = needy_config::profile::load(&needy_config::Source::text(profile_yaml.clone(), PathBuf::new()))?;
  let driver = driver::new(&actor::host(daemon), &session_profile.actor)?;
  let channels = driver.channels(dir);
  let guest_bin = actor::guest_needy(&daemon.config)?;

  let session_cancel = cancel.child_token();
  let tasks = TaskTracker::new();

  let guest_config = GuestConfig {
    run: session_id.to_string(),
    user: GuestUser {
      name: spec.user.name.clone(),
      uid: spec.user.uid,
      gid: spec.user.gid,
    },
    route_names: Vec::new(),
    hostname: format!("needy-{session_id}"),
    uptime_offset_secs: 0,
    mounts: Vec::new(),
    command: spec.command,
    env: spec.env.into_iter().collect(),
    cwd: spec.cwd,
    gateway_port: channels.gateway_port,
    sensors: Vec::new(),
    copy_in: Vec::new(),
    mirror_out: Vec::new(),
    sensor_faults: Vec::new(),
    tty: spec.tty,
    process_port: channels.process_port,
    wait_for_attach: spec.wait_for_attach,
    daemon_uid: daemon_uid(),
    daemon_gid: daemon_gid(),
    layers: spec.snapshot.as_ref().map(|s| s.layers.iter().map(ToString::to_string).collect()).unwrap_or_default(),
    image_path: spec.image_path,
    session: spec.session,
  };

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
  let gw_path = dir.join("cg.sock");
  let gw = Arc::new(gateway::Gateway::new(spec.network, spec.trace, Network::Actor, None, session_cancel.clone()));
  let gw_listener = tokio::net::UnixListener::bind(&gw_path).with_context(|| format!("cannot listen on {}", gw_path.display()))?;
  {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&gw_path, std::fs::Permissions::from_mode(0o600))
      .with_context(|| format!("cannot restrict {}", gw_path.display()))?;
  }
  tasks.spawn(gw.run(tokio_stream::wrappers::UnixListenerStream::new(gw_listener)));

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
  let layer = if spec.session.upper == UpperLayer::Kept && succeeded {
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

/// Resolves `run`'s `actor.image` when it is an [`Image::Definition`]: builds
/// any missing `run` layers, materializes the resulting snapshot, and installs
/// it on `run` via [`Run::set_boot`]. No-op when `actor.image` is absent or an
/// OCI image reference.
// TODO(images): auto-pulling a missing base VM image on `run create` comes
// with slice 3 of the images and run data spec ("Slices"); until then
// `actor::check_host` fails before this runs with the `needy image pull`
// advice.
// TODO(images): reading a commit layer from the image's lockfile and passing
// it to `needy_core::image::plan` comes with step 8 of the slice 1 plan.
pub(crate) async fn prepare(daemon: &Daemon, run: &Run) -> Result<()> {
  let Some(Image::Definition(definition)) = &run.profile.actor.image else {
    return Ok(());
  };
  let root = root_digest(daemon).await?;
  let plan = needy_core::image::plan(definition, &root, &run.profile.user, None);
  let mut layers = Vec::with_capacity(plan.steps.len());

  for step in &plan.steps {
    if run.cancel.is_cancelled() {
      bail!("the daemon shut down while building the image");
    }
    let (layer, cached, ms) = daemon.images.resolve_step(daemon, run, &layers, step).await?;
    let Input::Run(script) = &step.input;
    run.emit(&Event::ImageBuildStep(ImageBuildStep {
      key: step.key.to_string(),
      layer: layer.to_string(),
      script: script.clone(),
      cached,
      ms,
    }));
    layers.push(layer);
  }

  let store = daemon.images.store.clone();
  let (snapshot, layer_disks) = tokio::task::spawn_blocking(move || -> Result<_> {
    let snapshot = store.snapshot(&layers)?;
    let disks = store.materialize(&snapshot)?;
    Ok((snapshot, disks))
  })
  .await
  .context("materializing snapshot panicked")??;

  run.set_boot(Boot {
    snapshot: Some(snapshot),
    layer_disks,
    path: plan.path,
    env: plan.env,
    session: GuestSession::default(),
  });
  Ok(())
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
  tokio::task::spawn_blocking(move || -> Result<PlanDigest> {
    if let Some(manifest) = manifest::read(&dir)?
      && let Some(sha256) = manifest.sha256
      && let Ok(digest) = PlanDigest::parse(&format!("sha256:{sha256}"))
    {
      return Ok(digest);
    }
    let rootfs = dir.join("rootfs.ext4");
    let mut file = std::fs::File::open(&rootfs).with_context(|| format!("cannot open {} to hash base rootfs", rootfs.display()))?;
    let mut hasher = sha2::Sha256::new();
    std::io::copy(&mut file, &mut hasher).with_context(|| format!("cannot hash {}", rootfs.display()))?;
    let text = format!("sha256:{:x}", hasher.finalize());
    PlanDigest::parse(&text).map_err(anyhow::Error::from)
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
