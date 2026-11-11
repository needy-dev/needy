//! Actor runtimes: what starts an actor next to the daemon, waits for it and
//! stops it, and where the daemon meets it.
//!
//! The daemon serves the actor's guest API and gateway on endpoints the
//! runtime names, and reaches the process service the actor serves for a
//! driving program on a terminal the way the runtime says. The transport of
//! each (a vsock device's Unix sockets under Firecracker) is the runtime's
//! choice, not the daemon's.
//!
//! See `docs/ai/references/architecture/2026-11-10-container-actor-runtime-design.md`.

use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;

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
  // caller that prints the error with its causes says it once.
  // TODO(container-actor): a runtime that needs no KVM is designed in
  // `docs/ai/references/architecture/2026-11-10-container-actor-runtime-design.md`;
  // the message names it once it ships. The message itself names no
  // repository path: a user with only the binary cannot follow one.
  #[error(
    "this host has no usable KVM: cannot open {}: {error}; every run needs it for its Firecracker actor. Without KVM, commands outside runs still work: `needy exec --runner`, `ps`, `attach`, `kill` and `fs cp`",
    path.display()
  )]
  Kvm {
    path: PathBuf,
    error: std::io::Error,
  },
}

/// Where the daemon and one actor meet. Each runtime chooses the transport;
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
  /// under Firecracker.
  // TODO(container-runtime): the guest configuration carries ports only; a
  // runtime whose actor reaches its channels otherwise extends it (proto
  // first) with that runtime.
  pub gateway_port: u32,
  /// What the actor's init is told to serve its process service on.
  pub process_port: u32,
  /// The socket files these channels leave in the actor's directory, which
  /// the daemon removes when the actor ends.
  pub sockets: Vec<PathBuf>,
}

/// Where the daemon listens for an actor's connections on one channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
  /// A Unix socket on the daemon's machine. Firecracker forwards the guest's
  /// vsock connections to host port `P` to `v.sock_P`; a Linux container
  /// actor would have the socket bind-mounted.
  Unix(PathBuf),
  // TODO(container-runtime): loopback TCP with a per-run token, for runtimes
  // whose actors live in a VM that cannot reach a host Unix socket (Docker
  // Desktop on macOS and Windows, Apple's `container`, WSL, Hyper-V); added
  // with the first such runtime, see
  // `docs/ai/references/architecture/2026-11-10-container-actor-runtime-design.md`.
}

/// How the daemon connects to an actor's process service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminal {
  /// Firecracker's vsock device: connect to its socket `device`, then ask
  /// for guest port `port`.
  FirecrackerVsock { device: PathBuf, port: u32 },
  // TODO(container-runtime): a Unix socket bind-mounted into a Linux
  // container actor, and loopback TCP with a per-run token for VM-backed
  // container runtimes, with the runtimes that need them.
}

/// The actor's log in its directory `dir`: every runtime writes its actor's
/// console there, and `run logs` reads it.
pub fn log(dir: &Path) -> PathBuf {
  dir.join("console.log")
}

/// What runs actors on this host: Firecracker today.
// TODO(container-runtime): `container` (the Docker API through `bollard`:
// `runc` or `runsc` on Linux, Docker Desktop on macOS and Windows) is slice 2
// of the container actor runtime design; `apple-container`, then `wsl` and
// `hyper-v`, come later on the owner's word.
pub trait ActorRuntime: Send + Sync {
  /// The runtime's name, as a profile's `actor.runtime` and `state.db`
  /// spell it.
  fn name(&self) -> &'static str;

  /// Fails unless this host can run the runtime's actors. A create checks
  /// this before it acquires anything.
  fn check_host(&self) -> Result<(), Error>;

  /// The channels of an actor whose directory is `dir`. The daemon serves
  /// them before it starts the actor.
  fn channels(&self, dir: &Path) -> Channels;

  /// Starts an actor in directory `dir` with `recipe`'s identity, or a
  /// plain one without a recipe.
  fn start(&self, dir: &Path, recipe: Option<&CompiledRecipe>) -> Result<Box<dyn RunningActor>, Error>;
}

/// A started actor. The handle owns it: the actor ends on its own after its
/// driving program, or when the handle stops it.
#[async_trait::async_trait]
pub trait RunningActor: Send {
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
