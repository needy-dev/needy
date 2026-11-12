//! Actor drivers: what starts an actor next to the daemon, waits for it and
//! stops it, and where the daemon meets it. A profile's `actor.driver` names
//! one; each is named after the operating system whose isolation it relies
//! on.
//!
//! | `actor.driver` | Module | Isolation it relies on |
//! | --- | --- | --- |
//! | `linux-firecracker` | [`linux_firecracker`] | Linux KVM, through Firecracker |
//! | `linux-container` | [`linux_container`] | Linux namespaces and cgroups (`runc`) or gVisor (`runsc`), through a Docker-compatible engine |
//! | `macos-vz` | [`macos_vz`] | macOS Virtualization.framework, through vfkit |
//! | `macos-apple-container` | [`macos_apple_container`] | macOS Virtualization.framework, through Apple's `container` |
//!
//! Each driver answers in its own module, and this module is the one place
//! that picks a driver by its name:
//!
//! - [`detect`]: whether this host has the driver, for the runner's
//!   capabilities;
//! - [`Driver::check_host`] and [`Driver::identity_gaps`]: whether a
//!   profile's actor can run here and what of its recipe it presents
//!   (`profile check` and a create);
//! - [`reconcile`]: ending what a daemon that died left of an actor;
//! - [`new`]: the factory, for a profile's actor;
//! - [`facets`]: what the driver presents and how it is reached, and
//!   [`Driver::channels`]: where the daemon meets one actor.
//!
//! Drivers never depend on each other: what two of them share lives in
//! `crate::contract` (ports and channel paths), `crate::image` (what they
//! boot or run) and `crate::identity` (identity files from the host), or here.
//!
//! The daemon serves the actor's guest API and gateway on endpoints the
//! driver names, and reaches the process service the actor serves for a
//! driving program on a terminal the way the driver says. The transport of
//! each (a vsock device's Unix sockets under Firecracker, Unix sockets
//! bind-mounted into a container) is the driver's choice, not the daemon's.
//!
//! See `docs/ai/references/architecture/2026-11-10-container-actor-runtime-design.md`.

use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;

use needy_config::profile::{Actor, ActorDriver, User};
use needy_core::recipe::CompiledRecipe;

#[cfg(unix)]
pub mod linux_container;
pub mod linux_firecracker;
#[cfg(unix)]
pub mod macos_apple_container;
pub mod macos_vz;
// TODO(macos-native): the `macos-native` driver (Seatbelt, `sandbox-exec`)
// lives on the paused `poc/macos-native-actor` branch; it becomes
// `macos_native/` here only if the owner productizes it, see
// `docs/ai/references/architecture/2026-11-12-drivers-and-extension-points-design.md`.

/// What a driver could not do. The variants are neutral; what a driver's
/// tool or engine refused is [`Error::Driver`], with its message.
#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("actor image is missing {}; run `needy build`", .0.display())]
  Missing(PathBuf),
  #[error("cannot write {}", path.display())]
  Write {
    path: PathBuf,
    #[source]
    source: std::io::Error,
  },
  /// A workspace mount on the host side could not be made.
  #[error("cannot mount {target} on the host")]
  Mount {
    target: String,
    #[source]
    source: anyhow::Error,
  },
  /// The driver does not do what was asked.
  #[error("{0}")]
  Unsupported(String),
  /// What `driver`'s tool, engine or host refused or failed: Firecracker
  /// that does not start, a KVM that does not open, the engine, Apple's
  /// `container`. The message says it whole, causes included.
  #[error("{message}")]
  Driver {
    driver: ActorDriver,
    message: String,
  },
}

impl Error {
  /// What `driver` refused, as [`Error::Driver`].
  pub fn driver(driver: ActorDriver, message: impl Into<String>) -> Self {
    Self::Driver {
      driver,
      message: message.into(),
    }
  }
}

/// Where the daemon and one actor meet. Each driver chooses the transport;
/// the daemon serves or dials whatever it is given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channels {
  /// Where the daemon serves the run's API to the actor's init and proxy.
  pub guest: Endpoint,
  /// Where the daemon serves the actor's gateway.
  pub gateway: Endpoint,
  /// How the daemon reaches the process service the actor serves for a
  /// driving program on a terminal.
  pub terminal: Terminal,
  /// What the actor's init is told to reach its gateway on: a vsock port
  /// under Firecracker, 0 where init and the driver agree on a path.
  // NOTICE: a container actor's init finds its channels at fixed paths
  // (`/.needy/channels/*.sock`), as a VM's init dials the fixed vsock port
  // of the guest API; the guest configuration carries ports only.
  // TODO(container-runtime): loopback TCP with a per-run token (Docker
  // Desktop) needs the address and token in the guest configuration, proto
  // first, with the first such driver.
  pub gateway_port: u32,
  /// What the actor's init is told to serve its process service on.
  pub process_port: u32,
  /// The socket files these channels leave on the daemon's machine, which
  /// the daemon removes when the actor ends.
  pub sockets: Vec<PathBuf>,
}

/// Where the daemon listens for an actor's connections on one channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
  /// A Unix socket on the daemon's machine. Firecracker forwards the guest's
  /// vsock connections to host port `P` to `v.sock_P`; a Linux container
  /// actor has the socket's directory bind-mounted; Apple's `container`
  /// relays the socket file mounted into its VM over vsock.
  Unix(PathBuf),
  // TODO(container-runtime): loopback TCP with a per-run token, for runtimes
  // whose actors live in a VM that cannot reach a host Unix socket (Docker
  // Desktop on macOS and Windows, WSL, Hyper-V); added
  // with the first such driver, see
  // `docs/ai/references/architecture/2026-11-10-container-actor-runtime-design.md`.
}

/// How the daemon connects to an actor's process service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminal {
  /// A hybrid vsock device (Firecracker's): connect to its Unix socket
  /// `device`, then ask for guest port `port` (`CONNECT port`).
  HybridVsock { device: PathBuf, port: u32 },
  /// A Unix socket on the daemon's machine that reaches the one the actor's
  /// init binds: a Linux container actor's bind-mounted channels, or the
  /// socket Apple's `container` publishes from its VM.
  Unix(PathBuf),
  // TODO(container-runtime): loopback TCP with a per-run token for VM-backed
  // container runtimes, with the first such runtime.
}

/// What a driver presents and how it is reached: independent answers that
/// one `Setup` (guest, host, or guest mounts) packed together before the
/// drivers design.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Facets {
  pub capture: Capture,
  /// Who mounts the workspace into the actor.
  pub workspace: Side,
  pub identity: Identity,
  pub transport: Transport,
}

/// How an exec inside the actor is caught and handed to the proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capture {
  /// The guest kernel's `binfmt_needy`: every exec, by any path.
  Kernel,
  /// Shims made when the container image was built: a program created at
  /// run time and run directly is not caught (`profile check` warns).
  Shims,
  // TODO(macos-native): `FencedShims` (shims in a Seatbelt fence) comes with
  // the `macos-native` driver, if the owner productizes it.
}

/// Where something the actor presents is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
  /// By the actor's init, inside.
  Guest,
  /// By the driver, from the daemon's machine.
  Host,
}

/// Who presents the identity fields the daemon configures: an actor's init
/// is told only those its driver presents in the guest, and the daemon
/// applies the hwmon faults itself where the driver presents the sensors
/// from the host. `None` is a gap the driver declares in its
/// [`Driver::identity_gaps`].
// NOTICE: the other recipe fields (CPU, DMI, PCI, kernel) need no daemon
// configuration: the VMM or the identity files present them, or a gap says
// they are missing; they get a field here when something has to act on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
  pub hostname: Option<Side>,
  /// The hwmon sensors and the hwmon faults.
  pub hwmon: Option<Side>,
  /// The recipe's uptime offset.
  pub uptime: Option<Side>,
}

/// How the actor reaches its channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
  /// vsock ports, which the VMM bridges to Unix sockets on the daemon's
  /// machine. vsock has no file modes inside the VM.
  Vsock,
  /// Unix sockets mounted into the actor (a directory of them, or one file
  /// each), and for the terminal one the driver publishes. A mounted socket
  /// keeps its file mode there, and the driving program's proxy runs as the
  /// profile's user, so the daemon opens the guest socket and its directory
  /// to every user once it bound them; the run's directory around them stays
  /// private on the host.
  Socket,
  // TODO(container-runtime): loopback TCP with a per-run token, see
  // `Endpoint`.
}

/// The facets of `driver`.
// NOTICE: the table functions but `detect` are Unix only, as the container
// drivers and the daemon's actor role are; a runner on Windows detects that
// it has none of the drivers.
#[cfg(unix)]
pub fn facets(driver: ActorDriver) -> Facets {
  match driver {
    ActorDriver::LinuxFirecracker => linux_firecracker::FACETS,
    ActorDriver::LinuxContainer => linux_container::FACETS,
    ActorDriver::MacosVz => macos_vz::FACETS,
    ActorDriver::MacosAppleContainer => macos_apple_container::FACETS,
  }
}

/// What the daemon's machine gives every driver: where needy's VM actor
/// image is (`needy build`), and the KVM device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
  pub image: PathBuf,
  /// `linux_firecracker::KVM` outside tests.
  pub kvm: PathBuf,
}

/// The driver of `actor` (a profile's `actor:`) on `host`.
#[cfg(unix)]
pub fn new(host: &Host, actor: &Actor) -> Result<Box<dyn Driver>, Error> {
  Ok(match actor.driver {
    ActorDriver::LinuxFirecracker => Box::new(linux_firecracker::LinuxFirecracker::on(host)),
    ActorDriver::LinuxContainer => Box::new(linux_container::LinuxContainer::of(actor)?),
    ActorDriver::MacosVz => Box::new(macos_vz::MacosVz::on(host)),
    ActorDriver::MacosAppleContainer => Box::new(macos_apple_container::MacosAppleContainer::of(actor)),
  })
}

/// Whether this host has `driver`, or why not: a runner reports
/// `actor(DRIVER)` for each that it has. What depends on a profile (an OCI
/// runtime, an image in a store) is checked when a run is created. A driver
/// on the Docker-compatible engine reads the answer `engine` shares with the
/// executor drivers.
pub async fn detect(driver: ActorDriver, host: &Host, engine: &needy_core_executor::engine::Probe) -> Result<(), String> {
  match driver {
    ActorDriver::LinuxFirecracker => linux_firecracker::detect(host),
    #[cfg(unix)]
    ActorDriver::LinuxContainer => linux_container::detect(engine).await,
    ActorDriver::MacosVz => macos_vz::detect(host).await,
    #[cfg(unix)]
    ActorDriver::MacosAppleContainer => macos_apple_container::detect().await,
    #[cfg(not(unix))]
    ActorDriver::LinuxContainer | ActorDriver::MacosAppleContainer => {
      let _ = engine;
      Err(format!("{driver} actors run on Linux and macOS only"))
    }
  }
}

/// What a daemon that died left of one actor, as `state.db` has it.
#[derive(Debug, Clone, Copy)]
pub struct Leftover<'a> {
  pub run: &'a str,
  /// The actor's directory.
  pub dir: &'a Path,
  /// The host process that ran it.
  pub pid: Option<u32>,
}

/// Ends what a daemon that died left of an actor of `driver`, and says
/// whether it found anything to end. A restarted daemon does not adopt an
/// actor: it is no longer the actor's parent, so it cannot collect its exit;
/// the guest's connections to the old daemon broke; and the run's chaos
/// counters and trace sequence lived in the old daemon's memory, so an
/// adopted run would no longer replay deterministically.
#[cfg(unix)]
pub async fn reconcile(driver: ActorDriver, leftover: &Leftover<'_>) -> bool {
  match driver {
    ActorDriver::LinuxFirecracker => linux_firecracker::reconcile(leftover).await,
    ActorDriver::LinuxContainer => linux_container::reconcile(leftover).await,
    ActorDriver::MacosVz => macos_vz::reconcile(leftover).await,
    ActorDriver::MacosAppleContainer => macos_apple_container::reconcile(leftover).await,
  }
}

/// An executor directory mounted into the actor over SFTP, as the daemon
/// tells a driver that mounts on the host side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostMount {
  /// The path in the actor.
  pub target: String,
  pub executor: String,
  /// The directory on the executor.
  pub source: String,
  pub writable: bool,
  /// The executor's `sftp-server` command.
  pub sftp_command: Vec<String>,
}

/// What a driver starts an actor with.
#[derive(Debug, Clone, Copy)]
pub struct Start<'a> {
  /// The run's ID, which names a container actor.
  pub run: &'a str,
  /// The actor's directory on the daemon's machine.
  pub dir: &'a Path,
  /// The recipe's identity, or a plain machine without one.
  pub recipe: Option<&'a CompiledRecipe>,
  /// The profile's user, who owns the home directory and the mounts.
  pub user: &'a User,
  /// The workspace mounts, for a driver whose [`Facets::workspace`] is
  /// [`Side::Host`].
  pub mounts: &'a [HostMount],
}

/// Something of a recipe's identity, or of the machine an actor presents,
/// that a driver does not provide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gap {
  /// The identity field, or `None` for what is no recipe field (such as
  /// lxcfs's resource views).
  pub field: Option<needy_config::recipe::IdentityField>,
  pub message: String,
}

impl Gap {
  pub fn of(field: needy_config::recipe::IdentityField, message: impl Into<String>) -> Self {
    Self {
      field: Some(field),
      message: message.into(),
    }
  }
}

/// The vCPUs and memory of an actor of `recipe`, or of a plain 2 vCPU,
/// 2 GiB machine without one: the same for every driver.
pub(crate) fn size(recipe: Option<&CompiledRecipe>) -> (u32, u32) {
  recipe.map(|r| (r.vcpus.max(1), r.mem_mib.max(256))).unwrap_or((2, 2048))
}

/// The actor's log in its directory `dir`: every driver writes its actor's
/// console there, and `run logs` reads it.
pub fn log(dir: &Path) -> PathBuf {
  dir.join("console.log")
}

/// An actor driver: what runs actors on this host (`linux-firecracker`,
/// `linux-container`, `macos-vz`, `macos-apple-container`).
// TODO(container-runtime): `wsl` and `hyper-v` come later on the owner's
// word; Docker Desktop on macOS and Windows needs the loopback TCP transport
// (see `Endpoint`).
#[async_trait::async_trait]
pub trait Driver: Send + Sync {
  /// Which driver it is.
  fn kind(&self) -> ActorDriver;

  /// The driver's name, as a profile's `actor.driver` and `state.db`
  /// spell it.
  fn name(&self) -> &'static str {
    self.kind().as_str()
  }

  /// What runs the actor, as the trace and the report name it: the name,
  /// with what decides the isolation where the name does not.
  fn label(&self) -> String {
    self.name().to_string()
  }

  /// Fails unless this host can run the driver's actors. A create checks
  /// this before it acquires anything.
  async fn check_host(&self) -> Result<(), Error>;

  /// The channels of an actor whose directory is `dir`. The daemon serves
  /// them before it starts the actor.
  fn channels(&self, dir: &Path) -> Channels;

  /// Starts an actor as `start` says.
  async fn start(&self, start: &Start<'_>) -> Result<Box<dyn Running>, Error>;

  /// Sets the recipe's temperature sensors of the actor in `dir` to
  /// `temp_mc` millidegrees, as an hwmon fault does, for a driver that
  /// presents the sensors from the host ([`Identity::hwmon`]); in the guest
  /// the actor's init applies the fault itself.
  fn overheat(&self, dir: &Path, temp_mc: i32) -> Result<(), Error> {
    let _ = (dir, temp_mc);
    Err(Error::Unsupported("this actor's sensors are set inside it".into()))
  }

  /// What of `recipe` (or of a plain machine) this driver's actors on this
  /// host do not present. A Firecracker actor presents all of it.
  fn identity_gaps(&self, recipe: Option<&CompiledRecipe>) -> Vec<Gap> {
    let _ = recipe;
    Vec::new()
  }
}

// NOTICE: how long a reconcile waits for a leftover VM process to exit after
// `SIGKILL`; it is reaped by its new parent at once.
const VM_END_WAIT: Duration = Duration::from_secs(5);

/// Ends the VM process (Firecracker, vfkit) of a leftover actor, if it still
/// runs, and says whether it did.
// NOTICE: the PID is checked against the process's command line, which
// names `marker`, a file in the run's directory (Firecracker's `vm.json`,
// vfkit's pidfile), so a PID another process took since is left alone.
#[cfg(unix)]
pub(crate) async fn end_vm(leftover: &Leftover<'_>, marker: &Path, name: &str) -> bool {
  use sysinfo::{ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, Signal, System, UpdateKind};
  let run = leftover.run;
  let Some(pid) = leftover.pid else {
    return false;
  };
  let pid = sysinfo::Pid::from_u32(pid);
  let Ok(mut system) = System::new() else {
    tracing::warn!("run {run}: cannot read this machine's processes to find its actor's VM");
    return false;
  };
  let alive = |system: &mut System| {
    system.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always));
    system.process(pid).is_some_and(|p| p.status() != ProcessStatus::Zombie && p.cmd().iter().any(|arg| Path::new(arg) == marker))
  };
  if !alive(&mut system) {
    return false;
  }
  tracing::warn!("run {run}: ending its actor's VM ({name} {pid}), which a restarted control plane does not adopt");
  if !system.process(pid).and_then(|p| p.kill_with(Signal::Kill)).unwrap_or(false) {
    tracing::warn!("run {run}: cannot kill the VM process {pid}");
  }
  let deadline = tokio::time::Instant::now() + VM_END_WAIT;
  while alive(&mut system) {
    if tokio::time::Instant::now() >= deadline {
      tracing::warn!("run {run}: the VM process {pid} did not exit within {VM_END_WAIT:?} of SIGKILL");
      break;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
  }
  true
}

/// Stops `child`, the host process `name` that runs a VM, and waits until it
/// is gone: `SIGTERM` first, then `SIGKILL` if it is still running after
/// `grace`.
pub(crate) async fn stop_process(child: &mut tokio::process::Child, name: &str, grace: Duration) -> std::io::Result<ExitStatus> {
  // Signalling a process that was already waited for could reach another
  // process that took its ID.
  if let Some(status) = child.try_wait()? {
    return Ok(status);
  }
  #[cfg(unix)]
  if let Some(pid) = child.id() {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    match kill(Pid::from_raw(pid as i32), Signal::SIGTERM) {
      Ok(()) => {
        if let Ok(waited) = tokio::time::timeout(grace, child.wait()).await {
          return waited;
        }
        tracing::warn!("{name} {pid} did not stop within {grace:?} of SIGTERM; killing it");
      }
      // It exited between `try_wait` and the signal; `wait` below reaps it.
      Err(nix::errno::Errno::ESRCH) => {}
      Err(e) => return Err(e.into()),
    }
  }
  #[cfg(not(unix))]
  let _ = (name, grace);
  child.start_kill()?;
  child.wait().await
}

/// A started actor. The handle owns it: the actor ends on its own after its
/// driving program, or when the handle stops it.
#[async_trait::async_trait]
pub trait Running: Send {
  /// The ID of the host process that runs the actor.
  fn pid(&self) -> u32;
  fn vcpus(&self) -> u32;
  fn mem_mib(&self) -> u32;

  /// Waits until the actor ends.
  async fn wait(&mut self) -> std::io::Result<ExitStatus>;

  /// Stops the actor and waits until it is gone, forcibly once `grace` has
  /// passed.
  async fn stop(&mut self, grace: Duration) -> std::io::Result<ExitStatus>;
}
