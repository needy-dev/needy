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
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::sync::{Arc, Mutex};

#[cfg(unix)]
use anyhow::{Context, Result, bail};
#[cfg(unix)]
use needy_api_server::control::{Caller, ControlError};
#[cfg(unix)]
use needy_api_server::{Health, Listen, Server, create_private_dir};
#[cfg(unix)]
use needy_core::types::SessionState;
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
  /// The KVM device an actor's Firecracker opens:
  /// `needy_core_actor::host::vm::KVM` outside tests.
  pub kvm: PathBuf,
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
  /// and `kill`, and who started it, for `ps`.
  // TODO(persistence): this map grows until the daemon exits; slice 9
  // keeps processes in `state.db`.
  pub(crate) processes: Arc<Mutex<HashMap<String, relay::Known>>>,
  /// The daemon's own tasks: the local executor and the relays of commands
  /// outside any session.
  pub(crate) tasks: TaskTracker,
  /// Session creates in progress, which shutdown waits for.
  pub(crate) creates: TaskTracker,
  pub(crate) cancel: CancellationToken,
  /// Set when [`Daemon::shutdown`] begins, so a stream it ends says why.
  shutting_down: Arc<AtomicBool>,
}

#[cfg(unix)]
impl Daemon {
  /// Opens the trace store and starts the in-process local executor.
  pub async fn start(config: Config) -> Result<Self> {
    for dir in [&config.state_dir, &config.runtime_dir] {
      warn_if_shared(dir);
    }
    let sessions_dir = config.state_dir.join("sessions");
    create_private_dir(&sessions_dir).with_context(|| format!("cannot create {}", sessions_dir.display()))?;
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
      creates: TaskTracker::new(),
      cancel,
      shutting_down: Arc::default(),
    })
  }

  /// Serves the control API on `listen` until a termination signal, then
  /// shuts down with [`Daemon::shutdown`], also when serving failed. A
  /// second termination signal ends the shutdown where it is and fails,
  /// naming the sessions it did not clean up; the caller should then exit.
  pub async fn serve(&self, listen: &str, token: Option<String>) -> Result<()> {
    let termination = needy_api_server::termination::Termination::listen().context("cannot handle termination signals")?;
    let ended = termination
      .unless_forced(async {
        let served = self.serve_until(listen, token, termination.stop()).await;
        self.shutdown().await;
        served
      })
      .await;
    match ended {
      Some(served) => served,
      None => {
        let left: Vec<String> = self.sessions.live().iter().map(|s| s.id.clone()).collect();
        // NOTICE: the sessions' executors, VMs and sockets stay behind; no
        // daemon reconciles them until slice 9 keeps sessions in `state.db`.
        let message = if left.is_empty() {
          "exited at a second termination signal before the shutdown ended".to_string()
        } else {
          format!("exited at a second termination signal; these sessions were not cleaned up: {}", left.join(", "))
        };
        tracing::error!("{message}");
        bail!(message)
      }
    }
  }

  async fn serve_until(&self, listen: &str, token: Option<String>, stop: CancellationToken) -> Result<()> {
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

  /// Unwinds the sessions being created, stops every other session in the
  /// order they were created, then the daemon's own tasks and the local
  /// executor's processes, and waits for them.
  pub async fn shutdown(&self) {
    self.shutting_down.store(true, Ordering::SeqCst);
    // A create sees its cancelled token before its next step and unwinds
    // through its rollback. A create that finished meanwhile is running
    // and is stopped below.
    for session in self.sessions.live() {
      if session.state.get() == SessionState::Creating {
        session.cancel.cancel();
      }
    }
    self.creates.close();
    self.creates.wait().await;
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
impl Daemon {
  /// Why the daemon ends an open exec stream: its session stopped, or the
  /// daemon, which stops every session first, is shutting down.
  pub(crate) fn end_reason(&self) -> &'static str {
    if self.shutting_down.load(Ordering::SeqCst) {
      "daemon shutting down"
    } else {
      "session stopped"
    }
  }
}

#[cfg(unix)]
/// Warns about a state or runtime directory that other users can enter. The
/// daemon creates its directories 0700, but leaves the mode of one that
/// exists, which may be shared.
fn warn_if_shared(dir: &std::path::Path) {
  use std::os::unix::fs::PermissionsExt;
  if let Ok(metadata) = std::fs::metadata(dir)
    && metadata.permissions().mode() & 0o077 != 0
  {
    tracing::warn!(
      "{} is open to other users (mode {:o}); its sockets are not; consider chmod 700",
      dir.display(),
      metadata.permissions().mode() & 0o777
    );
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
