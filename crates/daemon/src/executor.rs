//! The executor role: this machine's processes and workspace filesystem,
//! served to the CLI or a control daemon until a termination signal.

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use needy_api_client::proto::v1::{FsLockMode, SessionRequest, SessionResponse};
use needy_api_server::control::{Caller, ControlError, FromRunner, Fs, ToRunner};
use needy_api_server::{Health, Listen, Server};
use needy_core_executor::fs::HostFs;
use needy_core_executor::process::Host;
use needy_core_executor::service::ExecutorService;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

// NOTICE: 64 responses in flight matches the client's `FsService` queue depth
// so multiplexed FUSE operations stay pipelined without unbounded buffering.
const FS_QUEUE: usize = 64;

/// Serves `FsService` rooted at each session's `FsMount.source` inside this
/// executor's filesystem namespace.
pub(crate) struct ExecutorFs {
  tasks: TaskTracker,
  cancel: CancellationToken,
}

impl ExecutorFs {
  pub(crate) fn new(tasks: TaskTracker, cancel: CancellationToken) -> Self {
    Self { tasks, cancel }
  }
}

#[async_trait::async_trait]
impl Fs for ExecutorFs {
  async fn session(&self, _caller: Caller, requests: FromRunner<SessionRequest>) -> Result<ToRunner<SessionResponse>, ControlError> {
    // NOTICE: return the outbound response stream immediately so the client's
    // `FsService.Session` RPC receives HTTP/2 response headers before sending
    // its initial `FsMount` request (avoiding a bidi-stream handshake deadlock).
    let (out_tx, out_rx) = mpsc::channel::<SessionResponse>(FS_QUEUE);
    let cancel = self.cancel.clone();
    self.tasks.spawn(async move {
      let _ = cancel
        .run_until_cancelled(HostFs::serve_stream(requests, out_tx, |mount| {
          let lock_mode = FsLockMode::try_from(mount.lock_mode).unwrap_or(FsLockMode::Local);
          HostFs::open(&mount.source, mount.writable, lock_mode)
        }))
        .await;
    });
    Ok(Box::pin(ReceiverStream::new(out_rx)))
  }
}

/// Serves `ProcessService` and `FsService` as the executor `name` on `listen`
/// (`unix:/path` or `HOST:PORT`); clients present `token` when one is set. On a
/// termination signal the listener stops, then the processes are killed. A
/// second termination signal ends that where it is and fails, naming the
/// processes still running; the caller should then exit. A Unix socket is
/// given to `socket_group`, mode 0660, when one is set.
pub async fn serve(listen: &str, name: String, token: Option<String>, socket_group: Option<u32>) -> Result<()> {
  let termination = needy_api_server::termination::Termination::listen().context("cannot handle termination signals")?;
  let health = Health {
    instance_id: uuid::Uuid::new_v4().to_string(),
    role: "executor",
  };
  let host = Host::default();
  let service = ExecutorService {
    host: host.clone(),
    name,
  };
  let tasks = TaskTracker::new();
  let fs = Arc::new(ExecutorFs::new(tasks.clone(), termination.stop()));
  let server = Server::new(health, Caller::Operator)
    .token(token)
    .socket_group(socket_group)
    .process_service(service)
    .fs(fs)
    .bind(&[Listen::parse(listen)])
    .await?;
  // The bound address, not `listen`: a port 0 is the one the system picked.
  for address in server.addresses() {
    tracing::info!("executor listening on {address}");
  }
  let ended = termination
    .unless_forced(async {
      let served = server.serve(termination.stop()).await;
      tasks.close();
      tasks.wait().await;
      host.shutdown().await;
      served
    })
    .await;
  match ended {
    Some(served) => Ok(served?),
    None => {
      let left: Vec<String> = host.list().iter().filter(|p| p.exit().is_none()).map(|p| p.id.clone()).collect();
      let message = if left.is_empty() {
        "exited at a second termination signal before the shutdown ended".to_string()
      } else {
        format!("exited at a second termination signal; these processes were not killed: {}", left.join(", "))
      };
      tracing::error!("{message}");
      bail!(message)
    }
  }
}
