//! The daemon: the control plane's policy, and the executor role. It decides
//! when to do what for which session; `core-actor` and `core-executor` are
//! the mechanisms. Listening, authentication and the wire are
//! `needy-api-server`'s; the daemon implements its domain traits.
//!
//! - `session`: sessions (create, get, list, stop) and their states.
//! - `pool`: executor instances by key, their reference counts and slots.
//! - `routing`: the `exec.decide` chain and the executors' path maps.
//! - `relay`: exec streams between a caller and an instance, through the
//!   stream filters installed for the session.
//! - `files`: copy-in and mirror-out on the session's side.
//! - `actor`: wires a session to its actor: the VM, the guest listener and
//!   the gateway.
//! - `guest`: the actor's side of its session (`GuestService`).
//! - [`executor`]: the executor role.
//!
//! Every long-lived task has an owner: the daemon or a session (whose actor
//! runs under a child token). An owner spawns its tasks into its
//! `TaskTracker` under its `CancellationToken`; stopping the owner cancels
//! them and waits for them.
//
// TODO(persistence): `store/` (`state.db` with sessions, instances and
// processes, reconciled on start) is slice 9 of the module layout redesign;
// until then sessions and processes live in memory.

pub mod executor;

#[cfg(unix)]
mod actor;
#[cfg(unix)]
mod files;
#[cfg(unix)]
mod guest;
#[cfg(unix)]
mod pool;
#[cfg(unix)]
mod relay;
#[cfg(unix)]
mod routing;
#[cfg(unix)]
mod session;

#[cfg(all(test, unix))]
mod testing;

#[cfg(unix)]
use std::collections::HashMap;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::sync::{Arc, Mutex};

#[cfg(unix)]
use anyhow::{Context, Result};
#[cfg(unix)]
use needy_api_server::control::{Caller, ControlError};
#[cfg(unix)]
use needy_api_server::{Health, Listen, Server};
#[cfg(unix)]
use needy_core_executor::process::Host;
#[cfg(unix)]
use needy_core_executor::provider::{self, Docker, Native};
#[cfg(unix)]
use needy_core_executor::service::ExecutorService;
#[cfg(unix)]
use needy_trace::sink::sqlite::Store;
#[cfg(unix)]
use tokio_util::sync::CancellationToken;
#[cfg(unix)]
use tokio_util::task::TaskTracker;

#[cfg(unix)]
use crate::pool::{Instance, Pool};
#[cfg(unix)]
use crate::session::Registry;

#[cfg(unix)]
/// Where the control daemon keeps its state and finds its images.
#[derive(Debug, Clone)]
pub struct Config {
  /// `trace.db` and the sessions' directories.
  pub state_dir: PathBuf,
  /// The local executor's socket.
  pub runtime_dir: PathBuf,
  /// The actor's `vmlinux`, `rootfs.ext4` and `firecracker`.
  pub actor_image: PathBuf,
  /// The image of Docker executors that name none.
  pub docker_image: String,
}

#[cfg(unix)]
/// The control daemon. Clones share it.
#[derive(Clone)]
pub struct Daemon {
  pub(crate) config: Arc<Config>,
  /// `trace.db`, shared by the sessions' SQLite sinks.
  pub(crate) traces: Store,
  pub(crate) sessions: Arc<Registry>,
  pub(crate) pool: Arc<Pool>,
  /// This machine, always available as the `local` executor.
  pub(crate) local: Arc<Instance>,
  /// The local executor's processes.
  host: Host,
  /// The instance each process the daemon has seen runs on, for `attach`
  /// and `kill`.
  // TODO(persistence): this map grows until the daemon exits; slice 9
  // keeps processes in `state.db`.
  pub(crate) processes: Arc<Mutex<HashMap<String, Arc<Instance>>>>,
  /// The daemon's own tasks: the local executor and the relays of commands
  /// outside any session.
  pub(crate) tasks: TaskTracker,
  pub(crate) cancel: CancellationToken,
}

#[cfg(unix)]
impl Daemon {
  /// Opens the trace store and starts the in-process local executor.
  pub async fn start(config: Config) -> Result<Self> {
    let sessions_dir = config.state_dir.join("sessions");
    std::fs::create_dir_all(&sessions_dir).with_context(|| format!("cannot create {}", sessions_dir.display()))?;
    let trace_db = config.state_dir.join("trace.db");
    let traces = Store::open(&trace_db).await.with_context(|| format!("cannot open {}", trace_db.display()))?;
    let tasks = TaskTracker::new();
    let cancel = CancellationToken::new();

    // This machine is always available as the `local` executor.
    let host = Host::default();
    let local_sock = config.runtime_dir.join("local.sock");
    let service = ExecutorService {
      host: host.clone(),
      name: "local".into(),
    };
    let health = Health {
      instance_id: uuid::Uuid::new_v4().to_string(),
      role: "executor",
    };
    // A socket that cannot bind, such as a path over `sun_path`, fails
    // start-up here with its path.
    let local_server = Server::new(health, Caller::Cli).process_service(service).bind(&[Listen::Unix(local_sock.clone())]).await?;
    let stop = cancel.clone();
    tasks.spawn(async move {
      if let Err(e) = local_server.serve(stop).await {
        tracing::error!("local executor: {e:#}");
      }
    });
    let local_client = provider::connect(&format!("unix:{}", local_sock.display())).await?;
    let native = Arc::new(Native::new(local_client));
    let docker = match Docker::new(provider::docker::Options {
      image: config.docker_image.clone(),
      state_dir: config.state_dir.clone(),
      allow: Vec::new(),
    }) {
      Ok(docker) => Some(docker),
      Err(e) => {
        tracing::warn!("docker executors are unavailable: {e}");
        None
      }
    };
    Ok(Self {
      config: Arc::new(config),
      traces,
      sessions: Arc::new(Registry::default()),
      local: Instance::machine(&native),
      pool: Arc::new(Pool::new(native, docker)),
      host,
      processes: Arc::default(),
      tasks,
      cancel,
    })
  }

  /// Serves the control API on `listen` until a termination signal. Call
  /// [`Daemon::shutdown`] afterwards.
  pub async fn serve(&self, listen: &str, token: Option<String>) -> Result<()> {
    let stop = needy_api_server::termination::token().context("cannot handle termination signals")?;
    let api = Arc::new(self.clone());
    let health = Health {
      instance_id: uuid::Uuid::new_v4().to_string(),
      role: "control",
    };
    let server = Server::new(health, Caller::Cli).token(token).sessions(api.clone()).processes(api).bind(&[Listen::parse(listen)]).await?;
    tracing::info!("control daemon listening on {listen}");
    server.serve(stop).await?;
    Ok(())
  }

  /// Stops every session in the order they were created, then the
  /// daemon's own tasks and the local executor's processes, and waits for
  /// them.
  pub async fn shutdown(&self) {
    for session in self.sessions.live() {
      if let Err(e) = self.stop_session(&session).await {
        tracing::warn!("session {}: {e}", session.id);
      }
    }
    self.cancel.cancel();
    self.host.shutdown().await;
    self.tasks.close();
    self.tasks.wait().await;
  }
}

#[cfg(unix)]
/// A failure of the daemon's own work, such as an invalid profile or a
/// copy-in that cannot be packed.
pub(crate) fn failed(e: anyhow::Error) -> ControlError {
  ControlError::Failed(format!("{e:#}"))
}

#[cfg(all(test, unix))]
#[path = "lib_test.rs"]
mod tests;
