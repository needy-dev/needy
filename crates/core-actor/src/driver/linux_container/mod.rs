//! The `linux-container` actor driver: the actor in an OCI container that
//! Docker runs with `runc` or gVisor's `runsc`, created through the Docker
//! API (`bollard`).
//!
//! The container gets no network but loopback. Its channels are Unix
//! sockets in the actor's directory (`channels/`), bind-mounted at
//! [`CHANNELS`]: the daemon serves the guest API and the gateway there, and
//! init serves the driving program's process service there. Workspace mounts
//! are sshfs on this machine, over the run's guest channel like a VM's, and
//! bind-mounted into the container. The container's stdout and stderr are
//! the actor's log.
//!
//! NOTICE: bind-mounted Unix sockets work only where the container and the
//! daemon share a kernel, or where the OCI runtime passes host sockets
//! through (gVisor's `--host-uds`); Docker Desktop runs containers in a VM
//! (`TODO(container-runtime)` on `driver::Endpoint`).
//!
//! See `docs/ai/references/architecture/2026-11-10-container-actor-runtime-design.md`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Duration;

use bollard::models::{ContainerCreateBody, HostConfig};
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use needy_config::profile::{Actor, ActorDriver, CONTAINER_IMAGE, ContainerActor, OciRuntime};

use needy_core::recipe::CompiledRecipe;

use crate::contract::{CHANNELS, GATEWAY_SOCKET, GUEST_SOCKET, PROCESS_SOCKET, channels_dir};
use crate::driver::{
  Capture, Channels, Driver, Endpoint, Error, Facets, Gap, HostMount, Identity, Leftover, Running, Side, Start, Terminal, Transport,
};
use crate::identity;
use crate::image::container::name;

/// Where the host-side workspace mounts of the actor in `dir` are.
fn mounts_dir(dir: &Path) -> PathBuf {
  dir.join("mounts")
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

// NOTICE: how long detection waits for the engine. A local socket answers in
// milliseconds; a Docker Desktop that is not running fails at once. 3 s
// bounds an engine that hangs without delaying a runner's join much.
const DETECT_WAIT: Duration = Duration::from_secs(3);

/// Whether this host can run `linux-container` actors: a Docker-compatible
/// engine answers. Whether it has the OCI runtime a profile names and the
/// container actor image is checked when a run is created.
// TODO(runner-actor): the capability does not carry the OCI runtimes; it
// needs to once actors run on runners other than the control host's own,
// which probes its engine directly.
pub async fn detect() -> Result<(), String> {
  // The OCI runtime and image do not matter for whether the engine answers.
  let driver = LinuxContainer::new(OciRuntime::Runc, CONTAINER_IMAGE.into());
  match tokio::time::timeout(DETECT_WAIT, driver.engine.runtimes()).await {
    Ok(runtimes) => runtimes.map(drop).map_err(|e| e.to_string()),
    Err(_) => Err(format!("no Docker-compatible engine answered within {DETECT_WAIT:?}")),
  }
}

/// Ends the container a daemon that died left, found by its run's name, and
/// detaches its host-side workspace mounts.
pub async fn reconcile(leftover: &Leftover<'_>) -> bool {
  // The OCI runtime and image do not matter for ending a container.
  let driver = LinuxContainer::new(OciRuntime::Runc, CONTAINER_IMAGE.into());
  tracing::warn!("run {}: ending its actor's container, which a restarted control plane does not adopt", leftover.run);
  driver.end_leftover(leftover.run, leftover.dir).await
}

/// What the engine refused or failed, as the driver's error.
fn refused(message: String) -> Error {
  Error::driver(ActorDriver::LinuxContainer, message)
}

/// The Docker daemon, as far as the driver uses it. A real boundary (a
/// container runtime), so tests stand in for it.
#[async_trait::async_trait]
pub trait Engine: Send + Sync {
  /// The OCI runtimes it has registered, each with the arguments Docker
  /// passes it (`runtimeArgs`).
  async fn runtimes(&self) -> Result<BTreeMap<String, Vec<String>>, Error>;
  async fn has_image(&self, image: &str) -> Result<bool, Error>;
  async fn create(&self, name: &str, body: ContainerCreateBody) -> Result<(), Error>;
  /// Starts the container and returns its first process's host PID.
  async fn start(&self, name: &str) -> Result<u32, Error>;
  /// The container's stdout and stderr from its start, until it exits.
  fn logs(&self, name: &str) -> BoxStream<'static, Result<bytes::Bytes, Error>>;
  /// Waits until the container exits and returns its exit code.
  async fn wait(&self, name: &str) -> Result<i64, Error>;
  /// `SIGTERM` to its first process, `SIGKILL` once `grace` has passed.
  async fn stop(&self, name: &str, grace: Duration) -> Result<(), Error>;
  /// Removes the container; one that is gone already is fine.
  async fn remove(&self, name: &str) -> Result<(), Error>;
}

/// The `linux-container` driver: actors in containers of `image`, run with
/// `oci_runtime`.
pub struct LinuxContainer {
  pub oci_runtime: OciRuntime,
  pub image: String,
  engine: Arc<dyn Engine>,
}

impl LinuxContainer {
  /// The driver on this machine's Docker daemon (`DOCKER_HOST`, else its
  /// default socket). Nothing is contacted until it is used; a Docker that
  /// cannot be reached fails each use, `check_host` first, with the reason.
  pub fn new(oci_runtime: OciRuntime, image: String) -> Self {
    let engine: Arc<dyn Engine> = match bollard::Docker::connect_with_defaults() {
      Ok(docker) => Arc::new(Docker(docker)),
      Err(e) => Arc::new(Unreachable(format!("no Docker daemon answers: {e}"))),
    };
    Self::with_engine(oci_runtime, image, engine)
  }

  /// The driver of `actor`, whose `container:` options name the OCI runtime
  /// (`validate::profile` refuses a profile without it first).
  pub fn of(actor: &Actor) -> Result<Self, Error> {
    let Some(ContainerActor {
      oci_runtime: Some(oci_runtime),
      image,
    }) = &actor.container
    else {
      return Err(Error::Unsupported("actor.container.oci_runtime is missing".into()));
    };
    Ok(Self::new(*oci_runtime, image.clone()))
  }

  pub fn with_engine(oci_runtime: OciRuntime, image: String, engine: Arc<dyn Engine>) -> Self {
    Self {
      oci_runtime,
      image,
      engine,
    }
  }

  /// Ends the container a previous daemon started for run `run`, whose
  /// directory is `dir`, and detaches its workspace mounts. Says whether a
  /// container was there.
  pub async fn end_leftover(&self, run: &str, dir: &Path) -> bool {
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
    for target in leftover_mounts(dir) {
      if let Err(e) = detach(&target) {
        tracing::warn!("run {run}: {e:#}");
      }
    }
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
    let mut all_binds = vec![format!("{}:{CHANNELS}", channels_dir(start.dir).display())];
    all_binds.extend(binds);
    // Docker refuses more CPUs than the host has; the recipe's count still
    // shows where the identity files present it.
    let host_cpus = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
    let body = ContainerCreateBody {
      image: Some(self.image.clone()),
      entrypoint: Some(vec!["/.needy/init".into()]),
      cmd: Some(Vec::new()),
      // Init sets the actor up as root and runs the driving program as the
      // profile's user.
      user: Some("0:0".into()),
      hostname: start.recipe.and_then(|r| r.hostname.clone()),
      labels: Some(HashMap::from([
        ("needy.run".to_string(), start.run.to_string()),
        ("needy.role".to_string(), "actor".to_string()),
      ])),
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

  /// Mounts each of `mounts` on this machine in the actor's directory `dir`,
  /// for user `uid`:`gid`, through the run's guest channel, and returns the
  /// mount points with their binds into the container.
  async fn mount(dir: &Path, mounts: &[HostMount], uid: u32, gid: u32) -> Result<Vec<(PathBuf, String)>, Error> {
    if mounts.is_empty() {
      return Ok(Vec::new());
    }
    #[cfg(target_os = "linux")]
    {
      let guest = format!("unix:{}", channels_dir(dir).join(GUEST_SOCKET).display());
      let client = needy_api_client::Client::connect(&guest).await.map_err(|e| Error::Mount {
        target: guest.clone(),
        source: e.into(),
      })?;
      let mut made = Vec::new();
      for (i, m) in mounts.iter().enumerate() {
        let point = mounts_dir(dir).join(i.to_string());
        let mount = crate::sshfs::Mount {
          target: point.display().to_string(),
          executor: m.executor.clone(),
          source: m.source.clone(),
          writable: m.writable,
          sftp_command: m.sftp_command.clone(),
        };
        if let Err(source) = crate::sshfs::mount(&client, &mount, uid, gid).await {
          undo(&made);
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
      let _ = (dir, uid, gid);
      Err(Error::Unsupported("a container actor's workspace mounts need Linux (sshfs on the daemon's machine)".into()))
    }
  }
}

/// Detaches the host-side mounts `made`, as far as it can.
fn undo(made: &[(PathBuf, String)]) {
  for (point, _) in made {
    if let Err(e) = detach(point) {
      tracing::warn!("{e:#}");
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

/// The mount points a daemon left in the actor's directory `dir` that are
/// still mounted.
fn leftover_mounts(dir: &Path) -> Vec<PathBuf> {
  let Ok(entries) = std::fs::read_dir(mounts_dir(dir)) else {
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
    let runtimes = self.engine.runtimes().await?;
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
    if !self.engine.has_image(&self.image).await? {
      return Err(refused(format!("the container actor image {} is missing; run `needy build --linux-container`", self.image)));
    }
    Ok(())
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
    let user = start.user;
    let host = identity::Host::read(Path::new("/"));
    let mut binds = identity::prepare(start.dir, start.recipe, crate::driver::size(start.recipe).0, &host, self.oci_runtime)?;
    let mounts = Self::mount(start.dir, start.mounts, user.uid, user.gid).await?;
    binds.extend(mounts.iter().map(|(_, bind)| bind.clone()));
    let launch = self.launch(start, binds);
    let points: Vec<PathBuf> = mounts.into_iter().map(|(point, _)| point).collect();
    let started = async {
      // A container left by an earlier daemon would block the name.
      self.engine.remove(&launch.name).await?;
      self.engine.create(&launch.name, launch.body.clone()).await?;
      // The log follows from the container's creation, so its first line
      // is kept even when it exits at once.
      let log = crate::driver::log(start.dir);
      let file = tokio::fs::File::create(&log).await.map_err(|source| Error::Write { path: log, source })?;
      let pid = self.engine.start(&launch.name).await?;
      Ok::<_, Error>((pid, file))
    };
    let (pid, file) = match started.await {
      Ok(started) => started,
      Err(e) => {
        if let Err(removed) = self.engine.remove(&launch.name).await {
          tracing::warn!("cannot remove {} after a failed start: {removed}", launch.name);
        }
        undo(&points.iter().map(|p| (p.clone(), String::new())).collect::<Vec<_>>());
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
      log: Some(log),
      ended: None,
    }))
  }
}

/// Copies the container's output into its log file until it exits.
async fn copy_log(mut logs: BoxStream<'static, Result<bytes::Bytes, Error>>, mut file: tokio::fs::File) {
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
    undo(&self.mounts.iter().map(|p| (p.clone(), String::new())).collect::<Vec<_>>());
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

/// A Docker daemon that cannot be reached, and why.
struct Unreachable(String);

#[async_trait::async_trait]
impl Engine for Unreachable {
  async fn runtimes(&self) -> Result<BTreeMap<String, Vec<String>>, Error> {
    Err(refused(self.0.clone()))
  }

  async fn has_image(&self, _image: &str) -> Result<bool, Error> {
    Err(refused(self.0.clone()))
  }

  async fn create(&self, _name: &str, _body: ContainerCreateBody) -> Result<(), Error> {
    Err(refused(self.0.clone()))
  }

  async fn start(&self, _name: &str) -> Result<u32, Error> {
    Err(refused(self.0.clone()))
  }

  fn logs(&self, _name: &str) -> BoxStream<'static, Result<bytes::Bytes, Error>> {
    futures_util::stream::iter([Err(refused(self.0.clone()))]).boxed()
  }

  async fn wait(&self, _name: &str) -> Result<i64, Error> {
    Err(refused(self.0.clone()))
  }

  async fn stop(&self, _name: &str, _grace: Duration) -> Result<(), Error> {
    Err(refused(self.0.clone()))
  }

  async fn remove(&self, _name: &str) -> Result<(), Error> {
    Err(refused(self.0.clone()))
  }
}

/// The Docker daemon through `bollard`.
struct Docker(bollard::Docker);

fn docker_error(e: bollard::errors::Error) -> Error {
  refused(format!("docker: {e}"))
}

/// Whether `e` is Docker's 404, a container or image that does not exist.
fn not_found(e: &bollard::errors::Error) -> bool {
  matches!(
    e,
    bollard::errors::Error::DockerResponseServerError {
      status_code: 404,
      ..
    }
  )
}

#[async_trait::async_trait]
impl Engine for Docker {
  async fn runtimes(&self) -> Result<BTreeMap<String, Vec<String>>, Error> {
    let info = self.0.info().await.map_err(|e| refused(format!("no Docker daemon answers: {e}")))?;
    Ok(info.runtimes.unwrap_or_default().into_iter().map(|(name, runtime)| (name, runtime.runtime_args.unwrap_or_default())).collect())
  }

  async fn has_image(&self, image: &str) -> Result<bool, Error> {
    match self.0.inspect_image(image).await {
      Ok(_) => Ok(true),
      Err(e) if not_found(&e) => Ok(false),
      Err(e) => Err(docker_error(e)),
    }
  }

  async fn create(&self, name: &str, body: ContainerCreateBody) -> Result<(), Error> {
    let options = bollard::query_parameters::CreateContainerOptionsBuilder::default().name(name).build();
    self.0.create_container(Some(options), body).await.map(drop).map_err(docker_error)
  }

  async fn start(&self, name: &str) -> Result<u32, Error> {
    self.0.start_container(name, None).await.map_err(docker_error)?;
    let inspected = self.0.inspect_container(name, None).await.map_err(docker_error)?;
    Ok(inspected.state.and_then(|s| s.pid).and_then(|pid| u32::try_from(pid).ok()).unwrap_or(0))
  }

  fn logs(&self, name: &str) -> BoxStream<'static, Result<bytes::Bytes, Error>> {
    let options = bollard::query_parameters::LogsOptionsBuilder::default().follow(true).stdout(true).stderr(true).build();
    self.0.logs(name, Some(options)).map(|output| output.map(|o| o.into_bytes()).map_err(docker_error)).boxed()
  }

  async fn wait(&self, name: &str) -> Result<i64, Error> {
    let options = bollard::query_parameters::WaitContainerOptionsBuilder::default().condition("not-running").build();
    let mut waited = self.0.wait_container(name, Some(options));
    match waited.next().await {
      Some(Ok(response)) => Ok(response.status_code),
      // NOTICE: bollard reports a non-zero exit as this error, with the
      // code (bollard 0.21.1 `src/container.rs`, `wait_container`).
      Some(Err(bollard::errors::Error::DockerContainerWaitError { code, .. })) => Ok(code),
      Some(Err(e)) => Err(docker_error(e)),
      None => Err(refused(format!("docker: the wait for {name} ended without its exit"))),
    }
  }

  async fn stop(&self, name: &str, grace: Duration) -> Result<(), Error> {
    let options = bollard::query_parameters::StopContainerOptionsBuilder::default().t(grace.as_secs() as i32).build();
    match self.0.stop_container(name, Some(options)).await {
      // 304: it was not running.
      Ok(())
      | Err(bollard::errors::Error::DockerResponseServerError {
        status_code: 304, ..
      }) => Ok(()),
      Err(e) => Err(docker_error(e)),
    }
  }

  async fn remove(&self, name: &str) -> Result<(), Error> {
    let options = bollard::query_parameters::RemoveContainerOptionsBuilder::default().force(true).build();
    match self.0.remove_container(name, Some(options)).await {
      // 404: no such container; 409: its removal is in progress.
      Ok(())
      | Err(bollard::errors::Error::DockerResponseServerError {
        status_code: 404 | 409,
        ..
      }) => Ok(()),
      Err(e) => Err(docker_error(e)),
    }
  }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
