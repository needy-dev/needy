//! The `container` actor runtime: the actor in an OCI container that
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
//! (`TODO(container-runtime)` on `runtime::Endpoint`).
//!
//! See `docs/ai/references/architecture/2026-11-10-container-actor-runtime-design.md`.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Duration;

use bollard::models::{ContainerCreateBody, HostConfig};
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use needy_config::profile::OciRuntime;

use super::runtime::{ActorRuntime, Channels, Endpoint, Error, HostMount, RunningActor, Setup, Start, Terminal};

/// Where the actor's channels are in the container.
pub const CHANNELS: &str = "/.needy/channels";
/// The daemon's guest API, in the channels directory.
pub const GUEST_SOCKET: &str = "guest.sock";
/// The actor gateway, in the channels directory.
pub const GATEWAY_SOCKET: &str = "gateway.sock";
/// The driving program's process service, which init serves.
pub const PROCESS_SOCKET: &str = "process.sock";

/// The channels directory in the actor's directory `dir`.
pub fn channels_dir(dir: &Path) -> PathBuf {
  dir.join("channels")
}

/// Where the host-side workspace mounts of the actor in `dir` are.
fn mounts_dir(dir: &Path) -> PathBuf {
  dir.join("mounts")
}

/// The container of run `run`'s actor.
pub fn name(run: &str) -> String {
  format!("needy-actor-{run}")
}

/// The Docker daemon, as far as the runtime uses it. A real boundary (a
/// container runtime), so tests stand in for it.
#[async_trait::async_trait]
pub trait Engine: Send + Sync {
  /// The OCI runtimes it has registered.
  async fn runtimes(&self) -> Result<BTreeSet<String>, Error>;
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

/// The `container` runtime: actors in containers of `image`, run with
/// `oci_runtime`.
pub struct Container {
  pub oci_runtime: OciRuntime,
  pub image: String,
  engine: Arc<dyn Engine>,
}

impl Container {
  /// The runtime on this machine's Docker daemon (`DOCKER_HOST`, else its
  /// default socket). Nothing is contacted until it is used.
  pub fn new(oci_runtime: OciRuntime, image: String) -> Result<Self, Error> {
    let docker = bollard::Docker::connect_with_defaults().map_err(|e| Error::Docker(format!("cannot reach Docker: {e}")))?;
    Ok(Self::with_engine(oci_runtime, image, Arc::new(Docker(docker))))
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
    let (vcpus, mem_mib) = start.recipe.map(|r| (r.vcpus.max(1), r.mem_mib.max(256))).unwrap_or((2, 2048));
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
impl ActorRuntime for Container {
  fn name(&self) -> &'static str {
    "container"
  }

  async fn check_host(&self) -> Result<(), Error> {
    let runtimes = self.engine.runtimes().await?;
    let oci = self.oci_runtime.as_str();
    if !runtimes.contains(oci) {
      let how = match self.oci_runtime {
        // Reference: https://gvisor.dev/docs/user_guide/install/ (no
        // versioned page; read 2026-11-11): `runsc install` registers it.
        OciRuntime::Runsc => "; install gVisor and register it with `runsc install`, see https://gvisor.dev/docs/user_guide/install/",
        OciRuntime::Runc => "",
      };
      let known: Vec<&str> = runtimes.iter().map(String::as_str).collect();
      return Err(Error::Docker(format!("Docker has no {oci} runtime (it has {}){how}", known.join(", "))));
    }
    if !self.engine.has_image(&self.image).await? {
      return Err(Error::Docker(format!("the container actor image {} is missing; run `needy build --container`", self.image)));
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
      guest_for_every_user: true,
    }
  }

  fn setup(&self) -> Setup {
    Setup::Host
  }

  async fn start(&self, start: &Start<'_>) -> Result<Box<dyn RunningActor>, Error> {
    let user = start.user;
    let mounts = Self::mount(start.dir, start.mounts, user.uid, user.gid).await?;
    let launch = self.launch(start, mounts.iter().map(|(_, bind)| bind.clone()).collect());
    let points: Vec<PathBuf> = mounts.into_iter().map(|(point, _)| point).collect();
    let started = async {
      // A container left by an earlier daemon would block the name.
      self.engine.remove(&launch.name).await?;
      self.engine.create(&launch.name, launch.body.clone()).await?;
      // The log follows from the container's creation, so its first line
      // is kept even when it exits at once.
      let log = super::runtime::log(start.dir);
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
    Ok(Box::new(Running {
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
struct Running {
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

impl Running {
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
impl RunningActor for Running {
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

/// The Docker daemon through `bollard`.
struct Docker(bollard::Docker);

fn docker_error(e: bollard::errors::Error) -> Error {
  Error::Docker(format!("docker: {e}"))
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
  async fn runtimes(&self) -> Result<BTreeSet<String>, Error> {
    let info = self.0.info().await.map_err(|e| Error::Docker(format!("no Docker daemon answers: {e}")))?;
    Ok(info.runtimes.unwrap_or_default().into_keys().collect())
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
      None => Err(Error::Docker(format!("docker: the wait for {name} ended without its exit"))),
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
#[path = "container_test.rs"]
mod tests;
