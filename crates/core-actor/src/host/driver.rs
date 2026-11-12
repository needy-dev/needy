//! Actor drivers: what starts an actor next to the daemon, waits for it and
//! stops it, and where the daemon meets it.
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

use needy_config::profile::User;
use needy_core::recipe::CompiledRecipe;

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
  #[error("cannot start firecracker")]
  Spawn(#[source] std::io::Error),
  // NOTICE: the OS error is part of the message and not its `source`, so a
  // caller that prints the error with its causes says it once. The message
  // names no repository path: a user with only the binary cannot follow one.
  #[error(
    "this host has no usable KVM: cannot open {}: {error}; a linux-firecracker actor needs it. Without KVM, use a linux-container actor (`actor: {{ driver: linux-container, container: {{ oci_runtime: runsc }} }}`); commands outside runs also work: `needy exec --runner`, `ps`, `attach`, `kill` and `fs cp`",
    path.display()
  )]
  Kvm {
    path: PathBuf,
    error: std::io::Error,
  },
  /// The `linux-container` driver's engine refused or failed a request.
  #[error("{0}")]
  Docker(String),
  /// Apple's `container` CLI is missing, or refused or failed a command.
  #[error("{0}")]
  AppleContainer(String),
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
  /// Whether every user in the actor connects to the guest endpoint's file
  /// itself: a socket bind-mounted into a container keeps its file mode
  /// there, and the driving program's proxy runs as the profile's user. The
  /// daemon then opens the socket and its directory to all users once it
  /// bound them; the run's directory around them stays private on the host.
  pub guest_for_every_user: bool,
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
  /// Firecracker's vsock device: connect to its socket `device`, then ask
  /// for guest port `port`.
  FirecrackerVsock { device: PathBuf, port: u32 },
  /// A Unix socket on the daemon's machine that reaches the one the actor's
  /// init binds: a Linux container actor's bind-mounted channels, or the
  /// socket Apple's `container` publishes from its VM.
  Unix(PathBuf),
  // TODO(container-runtime): loopback TCP with a per-run token for VM-backed
  // container runtimes, with the first such runtime.
}

/// Who sets up what the driving program sees beyond its own files: the
/// workspace mounts, and the recipe's hostname and sensors with their
/// faults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setup {
  /// The actor's init, inside: a VM has its own kernel to mount and fake
  /// sensors in.
  Guest,
  /// The driver, from the daemon's machine: a container gets its mounts,
  /// hostname and identity files from the host, since its init may not
  /// mount anything.
  Host,
  /// Both: the actor's init mounts the workspace and sets the hostname, as
  /// in a VM, and the driver presents the rest of the identity from the
  /// host, as for a container. Apple's `container`: its container is in a VM
  /// with FUSE, where init may mount, but whose default kernel has no fake
  /// sensors; and the Mac has no FUSE to mount on.
  GuestMounts,
}

impl Setup {
  /// Whether the actor's init mounts the workspace and sets the recipe's
  /// hostname.
  pub fn mounts_in_guest(self) -> bool {
    matches!(self, Self::Guest | Self::GuestMounts)
  }

  /// Whether the actor's init presents the recipe's sensors, applies hwmon
  /// faults and offsets the uptime.
  pub fn identity_in_guest(self) -> bool {
    self == Self::Guest
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
  /// The workspace mounts, for a driver whose [`Setup`] is `Host`.
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
  /// The driver's name, as a profile's `actor.driver` and `state.db`
  /// spell it.
  fn name(&self) -> &'static str;

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

  /// Who mounts the workspace and presents the recipe's hostname and
  /// sensors.
  fn setup(&self) -> Setup;

  /// Starts an actor as `start` says.
  async fn start(&self, start: &Start<'_>) -> Result<Box<dyn Running>, Error>;

  /// Sets the recipe's temperature sensors of the actor in `dir` to
  /// `temp_mc` millidegrees, as an hwmon fault does, for a driver whose
  /// [`Setup`] is `Host`; under `Guest` the actor's init applies the fault
  /// itself.
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
