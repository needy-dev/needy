//! The `linux-container` actor driver: the actor in an OCI container that
//! a Docker-compatible engine runs with `runc` or gVisor's `runsc`, created
//! through the engine client both `linux-container` drivers share
//! (`needy_core_executor::engine`).
//!
//! The container gets no network but loopback. Its channels are Unix
//! sockets in the actor's directory (`channels/`), bind-mounted at
//! [`CHANNELS`]: the daemon serves the guest API and the gateway there, and
//! init serves the driving program's process service there. Workspace mounts
//! are sshfs on this machine, over the run's guest channel like a VM's, and
//! bind-mounted into the container. The image is a base image: the guest
//! needy is bind-mounted read-only at [`GUEST`], and the image's
//! `needy.base-abi` label must be this needy's. The container's stdout and
//! stderr are the actor's log.
//!
//! NOTICE: bind-mounted Unix sockets work only where the container and the
//! daemon share a kernel, or where the OCI runtime passes host sockets
//! through (gVisor's `--host-uds`); Docker Desktop runs containers in a VM
//! (`TODO(container-runtime)` on `driver::Endpoint`).
//!
//! See `docs/ai/references/architecture/2026-11-10-container-actor-runtime-design.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use needy_config::profile::{Actor, ActorDriver, CONTAINER_IMAGE, ContainerActor, OciRuntime};
use needy_core_executor::engine::models::{ContainerCreateBody, HostConfig};
use needy_core_executor::engine::{self, Engine};

use needy_core::recipe::CompiledRecipe;
use needy_core::types::Capture;

use crate::contract::{CHANNELS, GATEWAY_SOCKET, GUEST, GUEST_SOCKET, INIT, PROCESS_SOCKET, channels_dir};
use crate::driver::{
  Channels, Driver, Endpoint, Error, Facets, Gap, HostMount, Identity, Leftover, Running, Side, Start, Terminal, Transport,
};
use crate::identity;
use crate::image::base;
use crate::image::container::{self, name};

/// What builds a container actor image of this needy's version.
const REBUILD: &str = "needy image build --linux-container";

/// Where the host-side workspace mounts of the actor in `dir` were before
/// they moved to [`Start::mounts_dir`].
// NOTICE: a run a daemon of v0.0.21 or earlier started still has its sshfs
// mounts here, inside the state directory, which a workspace such as `$HOME`
// contains. Reconcile detaches them; this read can go once no supported
// upgrade starts from v0.0.21 or earlier with runs left over.
fn legacy_mounts_dir(dir: &Path) -> PathBuf {
  dir.join("mounts")
}

/// Creates `dir`, the run's directory of mount points, and any missing
/// parents, private (0700) to the daemon's account: the points are the
/// workspace, which only the daemon and the engine it binds them for read.
#[cfg(target_os = "linux")]
fn create_mounts_dir(dir: &Path) -> std::io::Result<()> {
  use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
  let missing: Vec<&Path> = dir.ancestors().take_while(|d| !d.as_os_str().is_empty() && !d.exists()).collect();
  std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
  // The umask may have narrowed the mode; it is set again so it is 0700.
  for created in missing {
    std::fs::set_permissions(created, std::fs::Permissions::from_mode(0o700))?;
  }
  Ok(())
}

/// What a `linux-container` actor presents: shims made when its image was
/// built catch execs; the driver mounts the workspace and presents the
/// hostname and the sensors from the host, and the uptime offset is a gap;
/// its channels are Unix sockets mounted into the container.
// TODO(container-actor): a container actor's uptime is the container's
// (lxcfs's `/proc/uptime` when it is mounted); the recipe's offset needs a
// time namespace init may not create there.
pub const FACETS: Facets = Facets {
  capture: Capture::Shims,
  workspace: Side::Host,
  identity: Identity {
    hostname: Some(Side::Host),
    hwmon: Some(Side::Host),
    uptime: None,
  },
  transport: Transport::Socket,
};

/// Whether this host can run `linux-container` actors: it is Linux, and a
/// Docker-compatible engine answers, as `engine` found. Whether it has the
/// OCI runtime a profile names and the container actor image is checked when
/// a run is created.
// TODO(runner-actor): the capability does not carry the OCI runtimes; it
// needs to once actors run on runners other than the control host's own,
// which probes its engine directly.
// NOTICE: elsewhere the engine (Docker Desktop, OrbStack) runs containers in
// a Linux VM, which cannot reach the run's channel sockets on this host (the
// module's NOTICE). A `linux-container` executor there still works: it is
// the executor drivers' detection, not this one, that `needy image pull`
// asks for the executor image.
pub async fn detect(engine: &engine::Probe) -> Result<(), String> {
  if !cfg!(target_os = "linux") {
    return Err(
      "linux-container actors run on Linux hosts only: elsewhere a Docker-compatible engine runs containers in a VM, \
       which cannot reach the run's channel sockets on this host"
        .into(),
    );
  }
  engine.answer().await
}

/// Ends the container a daemon that died left, found by its run's name, and
/// detaches its host-side workspace mounts.
pub async fn reconcile(leftover: &Leftover<'_>) -> bool {
  // The OCI runtime and image do not matter for ending a container.
  let driver = LinuxContainer::new(OciRuntime::Runc, CONTAINER_IMAGE.into());
  tracing::warn!("run {}: ending its actor's container, which a restarted control plane does not adopt", leftover.run);
  driver.end_leftover(leftover).await
}

/// What the driver refused, as its error.
fn refused(message: String) -> Error {
  Error::driver(ActorDriver::LinuxContainer, message)
}

/// What the engine did not do, as the driver's error.
fn engine_error(e: engine::Error) -> Error {
  refused(e.to_string())
}

/// The `linux-container` driver: actors in containers of `image`, run with
/// `oci_runtime`.
pub struct LinuxContainer {
  pub oci_runtime: OciRuntime,
  pub image: String,
  seed: Option<u64>,
  engine: Arc<dyn Engine>,
}

impl LinuxContainer {
  /// The driver on this machine's engine (`DOCKER_HOST`, else its default
  /// socket). Nothing is contacted until it is used; an engine that cannot
  /// be reached fails each use, `check_host` first, with the reason.
  pub fn new(oci_runtime: OciRuntime, image: String) -> Self {
    Self::with_engine(oci_runtime, image, engine::local())
  }

  /// The driver of `actor`, whose `container:` options name the OCI runtime
  /// (`validate::profile` refuses a profile without it first).
  pub fn of(actor: &Actor) -> Result<Self, Error> {
    Self::of_with_engine(actor, engine::local())
  }

  /// Like [`Self::of`], using `engine` instead of the default local engine.
  pub fn of_with_engine(actor: &Actor, engine: Arc<dyn Engine>) -> Result<Self, Error> {
    let Some(ContainerActor {
      oci_runtime: Some(oci_runtime),
    }) = &actor.container
    else {
      return Err(Error::Unsupported("actor.container.oci_runtime is missing".into()));
    };
    let mut driver = Self::with_engine(*oci_runtime, actor.container_image(), engine);
    driver.seed = actor.seed.as_ref().map(|seed| match seed {
      needy_config::profile::Seed::Int(n) => *n,
      needy_config::profile::Seed::String(s) => needy_common::seed::from_str(s),
    });
    Ok(driver)
  }

  pub fn with_engine(oci_runtime: OciRuntime, image: String, engine: Arc<dyn Engine>) -> Self {
    Self {
      oci_runtime,
      image,
      seed: None,
      engine,
    }
  }

  /// Pulls the image, which the engine does not have, and returns what it
  /// records. Container images are pulled automatically, unlike VM images
  /// (owner decision D4 of the actor images design).
  async fn pull(&self) -> Result<engine::Image, Error> {
    tracing::info!("pulling the container actor image {}", self.image);
    if let Err(e) = self.engine.pull(&self.image).await {
      return Err(refused(container::unpullable(&self.image, &e.to_string(), REBUILD)));
    }
    self
      .engine
      .inspect_image(&self.image)
      .await
      .map_err(engine_error)?
      .ok_or_else(|| refused(container::unpullable(&self.image, "the engine pulled it but does not have it", REBUILD)))
  }

  /// Ends the container a previous daemon started for the run of
  /// `leftover`, detaches its workspace mounts and removes their mount
  /// points. Says whether a container was there.
  pub async fn end_leftover(&self, leftover: &Leftover<'_>) -> bool {
    let run = leftover.run;
    let name = name(run);
    let found = match self.engine.stop(&name, Duration::ZERO).await {
      Ok(()) => true,
      Err(e) => {
        tracing::debug!("run {run}: no container {name} to stop: {e}");
        false
      }
    };
    if let Err(e) = self.engine.remove(&name).await {
      tracing::warn!("run {run}: cannot remove {name}: {e}");
    }
    let mut points = leftover_mounts(leftover.mounts_dir);
    points.extend(leftover_mounts(&legacy_mounts_dir(leftover.dir)));
    for target in &points {
      if let Err(e) = detach(target) {
        tracing::warn!("run {run}: {e:#}");
      }
    }
    // Every point, mounted or not: a daemon that died between creating a
    // point and mounting it leaves it empty. The legacy points stay with
    // the run's directory, as before.
    let made: Vec<PathBuf> = std::fs::read_dir(leftover.mounts_dir)
      .map(|entries| entries.filter_map(|entry| entry.ok().map(|entry| entry.path())).collect())
      .unwrap_or_default();
    remove_points(&made, leftover.mounts_dir);
    found
  }

  /// The container for `start`, with the host-side mounts and identity
  /// files at `binds`.
  pub fn launch(&self, start: &Start<'_>, binds: Vec<String>) -> Launch {
    let (vcpus, mem_mib) = crate::driver::size(start.recipe);
    let user = start.user;
    let home = user.home();
    // NOTICE: the options are spelled out: Docker's tmpfs mounts default to
    // `noexec`, and the actor's writable directories allow programs, as a
    // VM's tmpfs directories do. The root file system is read-only, as a
    // VM's is.
    // Reference: https://docs.docker.com/engine/storage/tmpfs/ (no versioned
    // page; read 2026-11-11).
    let tmpfs = HashMap::from([
      ("/tmp".to_string(), "rw,exec,nosuid,nodev,mode=1777".to_string()),
      ("/var/tmp".to_string(), "rw,exec,nosuid,nodev,mode=1777".to_string()),
      ("/run".to_string(), "rw,exec,nosuid,nodev,mode=755".to_string()),
      (home, format!("rw,exec,nosuid,nodev,mode=755,uid={},gid={}", user.uid, user.gid)),
    ]);
    // The guest needy goes in read-only over the base image's placeholder.
    let mut all_binds = vec![
      format!("{}:{CHANNELS}", channels_dir(start.dir).display()),
      format!("{}:{GUEST}:ro", start.guest.display()),
    ];
    all_binds.extend(binds);
    // Docker refuses more CPUs than the host has; the recipe's count still
    // shows where the identity files present it.
    let host_cpus = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
    let image = start.snapshot_image.unwrap_or(&self.image);
    let body = ContainerCreateBody {
      image: Some(image.to_string()),
      entrypoint: Some(vec![INIT.into()]),
      cmd: Some(Vec::new()),
      // Init sets the actor up as root and runs the driving program as the
      // profile's user.
      user: Some("0:0".into()),
      hostname: start.recipe.and_then(|r| r.hostname.clone()),
      labels: Some(engine::labels(start.run, "actor")),
      host_config: Some(HostConfig {
        runtime: Some(self.oci_runtime.as_str().into()),
        network_mode: Some("none".into()),
        readonly_rootfs: Some(true),
        tmpfs: Some(tmpfs),
        binds: Some(all_binds),
        nano_cpus: Some(i64::from(vcpus.min(host_cpus)) * 1_000_000_000),
        memory: Some(i64::from(mem_mib) << 20),
        ..Default::default()
      }),
      ..Default::default()
    };
    Launch {
      name: name(start.run),
      body,
      vcpus,
      mem_mib,
    }
  }

  /// Starts a writable container session (for image build steps and
  /// interactive edit sessions when this host has no VM driver).
  ///
  /// Unlike [`Driver::start`], the container's root filesystem stays writable,
  /// `/etc/passwd` and `/etc/group` are not bind-mounted over the image,
  /// `/root` is not shadowed by a tmpfs, and the stopped container is kept on
  /// exit so the caller can commit its upper layer before calling
  /// [`SessionContainer::remove`].
  pub async fn start_session(&self, start: &Start<'_>) -> Result<SessionContainer, Error> {
    if !start.layer_disks.is_empty() {
      return Err(Error::Unsupported("linux-container sessions boot snapshots as assembled OCI images, not VM layer disks".into()));
    }
    let (vcpus, mem_mib) = crate::driver::size(start.recipe);
    let tmpfs = HashMap::from([
      ("/tmp".to_string(), "rw,exec,nosuid,nodev,mode=1777".to_string()),
      ("/var/tmp".to_string(), "rw,exec,nosuid,nodev,mode=1777".to_string()),
      ("/run".to_string(), "rw,exec,nosuid,nodev,mode=755".to_string()),
    ]);
    let binds = vec![
      format!("{}:{CHANNELS}", channels_dir(start.dir).display()),
      format!("{}:{GUEST}:ro", start.guest.display()),
    ];
    let host_cpus = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
    let image = start.snapshot_image.unwrap_or(&self.image);
    let container_name = name(start.run);
    let body = ContainerCreateBody {
      image: Some(image.to_string()),
      entrypoint: Some(vec![INIT.into()]),
      cmd: Some(Vec::new()),
      user: Some("0:0".into()),
      labels: Some(engine::labels(start.run, "actor")),
      host_config: Some(HostConfig {
        runtime: Some(self.oci_runtime.as_str().into()),
        network_mode: Some("none".into()),
        readonly_rootfs: Some(false),
        tmpfs: Some(tmpfs),
        binds: Some(binds),
        nano_cpus: Some(i64::from(vcpus.min(host_cpus)) * 1_000_000_000),
        memory: Some(i64::from(mem_mib) << 20),
        ..Default::default()
      }),
      ..Default::default()
    };
    let started = async {
      self.engine.remove(&container_name).await.map_err(engine_error)?;
      self.engine.create(&container_name, body).await.map_err(engine_error)?;
      let log = crate::driver::log(start.dir);
      let file = tokio::fs::File::create(&log).await.map_err(|source| Error::Write { path: log, source })?;
      let pid = self.engine.start(&container_name).await.map_err(engine_error)?;
      Ok::<_, Error>((pid, file))
    };
    let (pid, file) = match started.await {
      Ok(started) => started,
      Err(e) => {
        if let Err(removed) = self.engine.remove(&container_name).await {
          tracing::warn!("cannot remove {container_name} after a failed session start: {removed}");
        }
        return Err(e);
      }
    };
    let log = tokio::spawn(copy_log(self.engine.logs(&container_name), file));
    Ok(SessionContainer {
      name: container_name,
      engine: self.engine.clone(),
      pid,
      log: Some(log),
      ended: None,
    })
  }

  /// Mounts each of `mounts` on this machine in `mounts_dir`, for user
  /// `uid`:`gid`, through the guest channel of the actor in `dir`, and
  /// returns the mount points with their binds into the container.
  async fn mount(dir: &Path, mounts_dir: &Path, mounts: &[HostMount], uid: u32, gid: u32) -> Result<Vec<(PathBuf, String)>, Error> {
    if mounts.is_empty() {
      return Ok(Vec::new());
    }
    #[cfg(target_os = "linux")]
    {
      create_mounts_dir(mounts_dir).map_err(|source| Error::Write {
        path: mounts_dir.to_path_buf(),
        source,
      })?;
      let guest = format!("unix:{}", channels_dir(dir).join(GUEST_SOCKET).display());
      let client = match needy_api_client::Client::connect(&guest).await {
        Ok(client) => client,
        Err(e) => {
          remove_points(&[], mounts_dir);
          return Err(Error::Mount {
            target: guest.clone(),
            source: e.into(),
          });
        }
      };
      let mut made = Vec::new();
      for (i, m) in mounts.iter().enumerate() {
        let point = mounts_dir.join(i.to_string());
        let mount = crate::sshfs::Mount {
          target: point.display().to_string(),
          executor: m.executor.clone(),
          source: m.source.clone(),
          writable: m.writable,
          sftp_command: m.sftp_command.clone(),
        };
        if let Err(source) = crate::fs::mount_workspace(&client, &mount, uid, gid).await {
          let mut points: Vec<PathBuf> = made.into_iter().map(|(point, _)| point).collect();
          for made in &points {
            if let Err(e) = detach(made) {
              tracing::warn!("{e:#}");
            }
          }
          // The point that did not mount is empty, or gone.
          points.push(point);
          remove_points(&points, mounts_dir);
          return Err(Error::Mount {
            target: m.target.clone(),
            source,
          });
        }
        let mode = if m.writable { "rw" } else { "ro" };
        made.push((point.clone(), format!("{}:{}:{mode}", point.display(), m.target.trim_end_matches('/'))));
      }
      Ok(made)
    }
    #[cfg(not(target_os = "linux"))]
    {
      let _ = (dir, mounts_dir, uid, gid);
      Err(Error::Unsupported("a container actor's workspace mounts need Linux (sshfs on the daemon's machine)".into()))
    }
  }
}

/// Detaches the host-side mounts at `points` in `mounts_dir`, as far as it
/// can, and removes the points and `mounts_dir`.
fn undo(points: &[PathBuf], mounts_dir: &Path) {
  for point in points {
    if let Err(e) = detach(point) {
      tracing::warn!("{e:#}");
    }
  }
  remove_points(points, mounts_dir);
}

/// Removes the mount points `points`, then `mounts_dir`, each only when it
/// is empty: a point whose mount could not be detached is busy or still
/// shows the workspace, and is left, with the directory, for reconcile.
fn remove_points(points: &[PathBuf], mounts_dir: &Path) {
  for dir in points.iter().map(PathBuf::as_path).chain([mounts_dir]) {
    match std::fs::remove_dir(dir) {
      Ok(()) => {}
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
      Err(e) => tracing::warn!("cannot remove the mount point {}: {e}", dir.display()),
    }
  }
}

/// Detaches a host-side workspace mount.
fn detach(point: &Path) -> anyhow::Result<()> {
  #[cfg(target_os = "linux")]
  {
    crate::sshfs::detach(&point.display().to_string())
  }
  #[cfg(not(target_os = "linux"))]
  {
    let _ = point;
    Ok(())
  }
}

/// The mount points a daemon left in `mounts_dir` that are still mounted.
fn leftover_mounts(mounts_dir: &Path) -> Vec<PathBuf> {
  let Ok(entries) = std::fs::read_dir(mounts_dir) else {
    return Vec::new();
  };
  let mounted = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
  entries
    .filter_map(|entry| entry.ok().map(|entry| entry.path()))
    .filter(|point| mounted.lines().any(|line| line.split(' ').nth(1) == Some(&point.display().to_string())))
    .collect()
}

/// One container about to be created.
#[derive(Debug, Clone, PartialEq)]
pub struct Launch {
  pub name: String,
  pub body: ContainerCreateBody,
  pub vcpus: u32,
  pub mem_mib: u32,
}

#[async_trait::async_trait]
impl Driver for LinuxContainer {
  fn kind(&self) -> ActorDriver {
    ActorDriver::LinuxContainer
  }

  fn label(&self) -> String {
    format!("linux-container ({})", self.oci_runtime.as_str())
  }

  async fn check_host(&self) -> Result<(), Error> {
    let runtimes = self.engine.runtimes().await.map_err(engine_error)?;
    let oci = self.oci_runtime.as_str();
    let Some(args) = runtimes.get(oci) else {
      let how = match self.oci_runtime {
        // Reference: https://gvisor.dev/docs/user_guide/install/ (no
        // versioned page; read 2026-11-11): `runsc install` registers it.
        OciRuntime::Runsc => "; install gVisor and register it with `runsc install`, see https://gvisor.dev/docs/user_guide/install/",
        OciRuntime::Runc => "",
      };
      let known: Vec<&str> = runtimes.keys().map(String::as_str).collect();
      return Err(refused(format!("Docker has no {oci} runtime (it has {}){how}", known.join(", "))));
    };
    // NOTICE: gVisor keeps a container from host Unix sockets unless told
    // otherwise, and the actor's channels are host sockets: init connects
    // to the guest API and the gateway (`open`) and binds its process
    // service (`create`). Observed with runsc release-20261102.0: init could
    // not reach `guest.sock` until runsc ran with `--host-uds=all`.
    // Reference: `runsc flags` (`-host-uds`: none|open|create|all, default
    // none) and `runsc install --help` (`-- [args...]` become Docker's
    // `runtimeArgs`).
    if self.oci_runtime == OciRuntime::Runsc && !args.iter().any(|arg| arg == "--host-uds=all") {
      return Err(refused(format!(
        "Docker's runsc runtime keeps containers from host Unix sockets, which a container actor's channels are (its runtimeArgs are [{}]); \
         register it with `runsc install -- --host-uds=all` and `systemctl reload docker`",
        args.join(" ")
      )));
    }
    let image = match self.engine.inspect_image(&self.image).await.map_err(engine_error)? {
      Some(image) => image,
      None => self.pull().await?,
    };
    base::check(&self.image, image.labels.get(base::LABEL).map(String::as_str), &container::advice(&self.image, REBUILD))?;
    container::check_platform(&self.image, &image.os, &image.architecture, crate::image::guest::arch()).map_err(refused)
  }

  fn image(&self) -> String {
    self.image.clone()
  }

  fn channels(&self, dir: &Path) -> Channels {
    let channels = channels_dir(dir);
    let guest = channels.join(GUEST_SOCKET);
    let gateway = channels.join(GATEWAY_SOCKET);
    let process = channels.join(PROCESS_SOCKET);
    Channels {
      sockets: vec![guest.clone(), gateway.clone(), process.clone()],
      guest: Endpoint::Unix(guest),
      gateway: Endpoint::Unix(gateway),
      terminal: Terminal::Unix(process),
      gateway_port: 0,
      process_port: 0,
    }
  }

  fn overheat(&self, dir: &Path, temp_mc: i32) -> Result<(), Error> {
    identity::overheat(dir, temp_mc)
  }

  fn identity_gaps(&self, recipe: Option<&CompiledRecipe>) -> Vec<Gap> {
    identity::gaps(recipe, self.oci_runtime, &identity::Host::read(Path::new("/")))
  }

  async fn start(&self, start: &Start<'_>) -> Result<Box<dyn Running>, Error> {
    if !start.layer_disks.is_empty() {
      return Err(Error::Unsupported("linux-container actors boot snapshots as assembled OCI images, not VM layer disks".into()));
    }
    let user = start.user;
    let host = identity::Host::read(Path::new("/"));
    let fallback_machine_id = self.seed.map(needy_common::seed::machine_id);
    let mut binds = identity::prepare_with_machine_id(
      start.dir,
      start.recipe,
      fallback_machine_id.as_deref(),
      crate::driver::size(start.recipe).0,
      &host,
      self.oci_runtime,
    )?;
    // The profile's user in the account files: init may not mount here, and
    // the root is read-only, so they are bound from the host and init finds
    // them right.
    let account = needy_common::account::Account {
      name: &user.name,
      uid: user.uid,
      gid: user.gid,
    };
    let scratch = format!("{}-account", name(start.run));
    let accounts = if let Some(snapshot) = start.snapshot_image {
      needy_core_executor::account::binds_local(self.engine.as_ref(), snapshot, &scratch, &start.dir.join("account"), &account)
        .await
        .map_err(engine_error)?
    } else {
      needy_core_executor::account::binds(self.engine.as_ref(), &self.image, &scratch, &start.dir.join("account"), &account)
        .await
        .map_err(engine_error)?
    };
    binds.extend(accounts);
    let mounts = Self::mount(start.dir, start.mounts_dir, start.mounts, user.uid, user.gid).await?;
    binds.extend(mounts.iter().map(|(_, bind)| bind.clone()));
    let launch = self.launch(start, binds);
    let points: Vec<PathBuf> = mounts.into_iter().map(|(point, _)| point).collect();
    let started = async {
      // A container left by an earlier daemon would block the name.
      self.engine.remove(&launch.name).await.map_err(engine_error)?;
      self.engine.create(&launch.name, launch.body.clone()).await.map_err(engine_error)?;
      // The log follows from the container's creation, so its first line
      // is kept even when it exits at once.
      let log = crate::driver::log(start.dir);
      let file = tokio::fs::File::create(&log).await.map_err(|source| Error::Write { path: log, source })?;
      let pid = self.engine.start(&launch.name).await.map_err(engine_error)?;
      Ok::<_, Error>((pid, file))
    };
    let (pid, file) = match started.await {
      Ok(started) => started,
      Err(e) => {
        if let Err(removed) = self.engine.remove(&launch.name).await {
          tracing::warn!("cannot remove {} after a failed start: {removed}", launch.name);
        }
        undo(&points, start.mounts_dir);
        return Err(e);
      }
    };
    let log = tokio::spawn(copy_log(self.engine.logs(&launch.name), file));
    Ok(Box::new(Started {
      name: launch.name,
      engine: self.engine.clone(),
      pid,
      vcpus: launch.vcpus,
      mem_mib: launch.mem_mib,
      mounts: points,
      mounts_dir: start.mounts_dir.to_path_buf(),
      log: Some(log),
      ended: None,
    }))
  }
}

/// Copies the container's output into its log file until it exits.
async fn copy_log(mut logs: BoxStream<'static, Result<bytes::Bytes, engine::Error>>, mut file: tokio::fs::File) {
  use tokio::io::AsyncWriteExt;
  while let Some(chunk) = logs.next().await {
    match chunk {
      Ok(bytes) => {
        if let Err(e) = file.write_all(&bytes).await {
          tracing::warn!("cannot write the actor's log: {e}");
          return;
        }
      }
      Err(e) => {
        tracing::debug!("the actor's log ended: {e}");
        break;
      }
    }
  }
  if let Err(e) = file.flush().await {
    tracing::warn!("cannot write the actor's log: {e}");
  }
}

/// A running container actor. Its end detaches its mounts and removes it.
struct Started {
  name: String,
  engine: Arc<dyn Engine>,
  pid: u32,
  vcpus: u32,
  mem_mib: u32,
  /// The host-side mount points.
  mounts: Vec<PathBuf>,
  /// The directory of `mounts`, removed with them.
  mounts_dir: PathBuf,
  log: Option<tokio::task::JoinHandle<()>>,
  /// The status it ended with, once it was cleaned up.
  ended: Option<ExitStatus>,
}

impl Started {
  /// Cleans up after the container exited with `code`: its log, its mounts
  /// and the container itself.
  async fn finish(&mut self, code: i64) -> ExitStatus {
    if let Some(status) = self.ended {
      return status;
    }
    if let Some(log) = self.log.take() {
      // NOTICE: the log stream ends when the container exits; 2 s bounds a
      // Docker daemon that keeps it open.
      if tokio::time::timeout(Duration::from_secs(2), log).await.is_err() {
        tracing::debug!("{}: the log did not end within 2 s", self.name);
      }
    }
    undo(&self.mounts, &self.mounts_dir);
    if let Err(e) = self.engine.remove(&self.name).await {
      tracing::warn!("cannot remove {}: {e}", self.name);
    }
    let status = exit_status(code);
    self.ended = Some(status);
    status
  }
}

/// A container's exit code as a process's wait status: Docker reports a
/// signal death as 128 + the signal, as a shell does.
fn exit_status(code: i64) -> ExitStatus {
  use std::os::unix::process::ExitStatusExt;
  ExitStatus::from_raw(((code & 0xff) as i32) << 8)
}

#[async_trait::async_trait]
impl Running for Started {
  /// The host PID of the container's first process (init, or gVisor's
  /// sandbox).
  fn pid(&self) -> u32 {
    self.pid
  }

  fn vcpus(&self) -> u32 {
    self.vcpus
  }

  fn mem_mib(&self) -> u32 {
    self.mem_mib
  }

  async fn wait(&mut self) -> std::io::Result<ExitStatus> {
    if let Some(status) = self.ended {
      return Ok(status);
    }
    let code = self.engine.wait(&self.name).await.map_err(std::io::Error::other)?;
    Ok(self.finish(code).await)
  }

  async fn stop(&mut self, grace: Duration) -> std::io::Result<ExitStatus> {
    if let Some(status) = self.ended {
      return Ok(status);
    }
    if let Err(e) = self.engine.stop(&self.name, grace).await {
      tracing::warn!("cannot stop {}: {e}", self.name);
    }
    let code = match self.engine.wait(&self.name).await {
      Ok(code) => code,
      Err(e) => {
        tracing::warn!("cannot wait for {}: {e}", self.name);
        // Docker's code for a container killed by `SIGKILL`.
        137
      }
    };
    Ok(self.finish(code).await)
  }
}

/// A running writable container session (build step or interactive edit).
/// Unlike [`Started`], exiting does not remove the container so the caller can
/// commit its filesystem changes before calling [`SessionContainer::remove`].
pub struct SessionContainer {
  name: String,
  engine: Arc<dyn Engine>,
  pid: u32,
  log: Option<tokio::task::JoinHandle<()>>,
  ended: Option<ExitStatus>,
}

impl SessionContainer {
  pub fn container_name(&self) -> &str {
    &self.name
  }

  pub fn pid(&self) -> u32 {
    self.pid
  }

  async fn drain_log(&mut self, code: i64) -> ExitStatus {
    if let Some(status) = self.ended {
      return status;
    }
    if let Some(log) = self.log.take()
      && tokio::time::timeout(Duration::from_secs(2), log).await.is_err()
    {
      tracing::debug!("{}: the session log did not end within 2 s", self.name);
    }
    let status = exit_status(code);
    self.ended = Some(status);
    status
  }

  pub async fn wait(&mut self) -> std::io::Result<ExitStatus> {
    if let Some(status) = self.ended {
      return Ok(status);
    }
    let code = self.engine.wait(&self.name).await.map_err(std::io::Error::other)?;
    Ok(self.drain_log(code).await)
  }

  pub async fn stop(&mut self, grace: Duration) -> std::io::Result<ExitStatus> {
    if let Some(status) = self.ended {
      return Ok(status);
    }
    if let Err(e) = self.engine.stop(&self.name, grace).await {
      tracing::warn!("cannot stop {}: {e}", self.name);
    }
    let code = match self.engine.wait(&self.name).await {
      Ok(code) => code,
      Err(e) => {
        tracing::warn!("cannot wait for {}: {e}", self.name);
        137
      }
    };
    Ok(self.drain_log(code).await)
  }

  pub async fn remove(&self) {
    if let Err(e) = self.engine.remove(&self.name).await {
      tracing::warn!("cannot remove {}: {e}", self.name);
    }
  }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
