//! Disposable actor sessions for image build steps, sync merges, and
//! interactive edit sessions, plus the `Guest` and `Runs` adapters they serve.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use needy_api_server::control::{
  Caller, Chunks, ControlError, FaultAdded, Guest, GuestConfig, GuestControls, GuestCopy, GuestSession, GuestUser, Logs, NewRun,
  ProfileCheck, ProfileToCheck, Route, RunEvents, RunFaults, RunInfo, Runs, StopMode, Target, UpperLayer, WindowSize,
};
use needy_api_server::{Health, Listen, Server};
use needy_common::process::Exit;
use needy_common::sync::lock;
use needy_config::profile::{ActorDriver, CONTAINER_IMAGE, OciRuntime, User};
use needy_core::event::{ActorStop, Event, ExecDecide};
use needy_core::image::Digest;
use needy_core::intercept::exec::ExecContext;
use needy_core::network::Policy;
use needy_core::types::{Network, Origin, Outcome, Placement, Principal};
use needy_core_actor::driver;
use needy_core_actor::driver::linux_container::LinuxContainer;
use needy_core_actor::image::store::{Command as StoreCommand, LayerDisk, Snapshot};
use needy_core_actor::sidecar::gateway;
use needy_trace::Dispatch;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use super::container::commit_container_to_file;
use crate::guest::{UPPER_LAYER, keep_upper};
use crate::{Daemon, actor};

/// How long a cancelled build or edit session's VM gets to exit on SIGTERM
/// before SIGKILL.
pub(super) const SESSION_STOP_GRACE: Duration = Duration::from_secs(3);

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
  /// Uncompressed tar archives staged into the session via `Guest::fetch_files`
  /// as `(name, guest_path, tar_bytes)`.
  pub copy_in: Vec<(String, String, Vec<u8>)>,
  /// Directories inside the session mirrored back via `Guest::push_files` after
  /// the command exits, as `(name, guest_path)`.
  pub mirror_out: Vec<(String, String)>,
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
  /// Uncompressed tar archives mirrored back via `Guest::push_files`, keyed by
  /// mapping name.
  pub mirrored_out: HashMap<String, Vec<u8>>,
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
  let copy_in_archives: HashMap<String, Vec<u8>> = spec.copy_in.into_iter().map(|(name, _, bytes)| (name, bytes)).collect();
  let expected_mirror_out: HashSet<String> = spec.mirror_out.into_iter().map(|(name, _)| name).collect();
  let upper_setting = guest_config.session.upper;
  let guest_service = Arc::new(SessionGuest {
    config: guest_config,
    copy_in_archives,
    expected_mirror_out,
    mirror_out_archives: Mutex::default(),
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
    mirrored_out: guest_service.mirrored_out(),
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
  let copy_in_archives: HashMap<String, Vec<u8>> = spec.copy_in.into_iter().map(|(name, _, bytes)| (name, bytes)).collect();
  let expected_mirror_out: HashSet<String> = spec.mirror_out.into_iter().map(|(name, _)| name).collect();
  // Container sessions boot an assembled OCI image, not VM layer disks.
  guest_config.layers.clear();
  let upper_setting = guest_config.session.upper;
  let guest_service = Arc::new(SessionGuest {
    config: guest_config,
    copy_in_archives,
    expected_mirror_out,
    mirror_out_archives: Mutex::default(),
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
    mirrored_out: guest_service.mirrored_out(),
    console,
  })
}

pub(super) fn open_socket_permissions(path: &Path) -> Result<()> {
  use std::os::unix::fs::PermissionsExt;
  std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))
    .with_context(|| format!("cannot open permissions on {}", path.display()))?;
  if let Some(dir) = path.parent() {
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))
      .with_context(|| format!("cannot open permissions on {}", dir.display()))?;
  }
  Ok(())
}

pub(super) fn session_profile_yaml(driver: ActorDriver, user: &User, container_image: Option<&str>) -> String {
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

pub(super) fn serve_session_gateway(
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
  copy_in: Vec<GuestCopy>,
  mirror_out: Vec<GuestCopy>,
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
      copy_in: self
        .copy_in
        .iter()
        .map(|(name, path, _)| GuestCopy {
          name: name.clone(),
          path: path.clone(),
        })
        .collect(),
      mirror_out: self
        .mirror_out
        .iter()
        .map(|(name, path)| GuestCopy {
          name: name.clone(),
          path: path.clone(),
        })
        .collect(),
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
    machine_id: String::new(),
    uptime_offset_secs: 0,
    mounts: Vec::new(),
    command: parts.command,
    env: parts.env.into_iter().collect(),
    cwd: parts.cwd,
    gateway_port: channels.gateway_port,
    sensors: Vec::new(),
    copy_in: parts.copy_in,
    mirror_out: parts.mirror_out,
    sensor_faults: Vec::new(),
    tty: parts.tty,
    process_port: channels.process_port,
    wait_for_attach: parts.wait_for_attach,
    daemon_uid: daemon_uid(),
    daemon_gid: daemon_gid(),
    layers: parts.layer_digests,
    image_path: parts.image_path,
    session: parts.session,
    // Lifecycle hooks run only for normal runs, never in build or merge sessions.
    start_hooks: Vec::new(),
    stop_hooks: Vec::new(),
  }
}

/// What `GuestService` serves to an image or merge session's guest init.
pub(super) struct SessionGuest {
  pub(super) config: GuestConfig,
  pub(super) copy_in_archives: HashMap<String, Vec<u8>>,
  pub(super) expected_mirror_out: HashSet<String>,
  pub(super) mirror_out_archives: Mutex<HashMap<String, Vec<u8>>>,
  pub(super) dir: PathBuf,
  pub(super) trace: Dispatch,
  pub(super) exit: Mutex<Option<Exit>>,
  pub(super) cancel: CancellationToken,
}

impl SessionGuest {
  pub(super) fn exit(&self) -> Option<Exit> {
    lock(&self.exit).clone()
  }

  fn mirrored_out(&self) -> HashMap<String, Vec<u8>> {
    lock(&self.mirror_out_archives).clone()
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
    *lock(&self.exit) = Some(exit);
    Ok(())
  }

  async fn fetch_files(&self, _run: &str, name: &str) -> Result<Vec<u8>, ControlError> {
    self
      .copy_in_archives
      .get(name)
      .cloned()
      .ok_or_else(|| ControlError::NotFound(format!("no staged files named {name:?} in this image session")))
  }

  async fn push_files(&self, _run: &str, name: &str, archive: Vec<u8>) -> Result<(), ControlError> {
    if !self.expected_mirror_out.contains(name) {
      return Err(ControlError::Failed(format!("unexpected mirror-out {name:?} in this session")));
    }
    lock(&self.mirror_out_archives).insert(name.to_string(), archive);
    Ok(())
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
pub(super) struct EditSessionRuns {
  pub(super) commands: Arc<Mutex<Vec<StoreCommand>>>,
  pub(super) trace: Dispatch,
}

#[async_trait::async_trait]
impl Runs for EditSessionRuns {
  async fn decide(&self, _run: &str, ctx: ExecContext) -> Result<Route, ControlError> {
    lock(&self.commands).push(StoreCommand::new(ctx.argv.clone(), ctx.cwd.clone()));
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

pub(super) fn daemon_uid() -> u32 {
  // SAFETY: `geteuid` cannot fail.
  unsafe { libc::geteuid() }
}

pub(super) fn daemon_gid() -> u32 {
  // SAFETY: `getegid` cannot fail.
  unsafe { libc::getegid() }
}

pub(super) fn cleanup_session_dir(dir: &Path) {
  if let Err(e) = std::fs::remove_dir_all(dir)
    && e.kind() != std::io::ErrorKind::NotFound
  {
    tracing::warn!("cannot remove session directory {}: {e}", dir.display());
  }
}
