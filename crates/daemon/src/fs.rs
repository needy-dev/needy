//! Workspace filesystem sessions (`FsService`) on the control daemon.
//!
//! Routes each `FsService.Session` stream to either a connected remote runner
//! (over `RunnerService.FsTask`) or the local host filesystem (`HostFs`),
//! based on the executor named in the initial `FsMount` request.

use std::sync::Arc;

use futures_util::StreamExt as _;
use needy_api_client::proto::v1::{
  FsError, FsLockMode, FsTaskRequest, SessionRequest, SessionResponse, fs_task_request, session_request, session_response,
};
use needy_api_server::control::{Caller, ControlError, FromRunner, Fs, ToRunner};
use needy_core_executor::fs::{HostFs, errno};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

use crate::Daemon;
use crate::link::Link;
use crate::run::Run;

// NOTICE: 64 responses in flight matches the client's `FsService` queue depth
// so multiplexed FUSE operations stay pipelined without unbounded buffering.
const QUEUE: usize = 64;

enum Target {
  Local,
  Runner(Arc<Link>),
}

impl Daemon {
  /// Resolves the executor named in `FsMount` to either a connected remote
  /// runner or the local host filesystem.
  async fn resolve_fs_target(&self, run: Option<&Arc<Run>>, executor: &str) -> Result<Target, ControlError> {
    if let Some(run) = run
      && let Some(instance) = run.executor(executor)
    {
      if let Some(runner) = instance.on_runner() {
        let link = self.links.get(runner).ok_or_else(|| {
          ControlError::Unavailable(crate::link::not_connected(runner))
            .with_diagnostic("runner.not_connected", [("runner".into(), runner.into())])
        })?;
        return Ok(Target::Runner(link));
      }
      return Ok(Target::Local);
    }

    if !executor.is_empty()
      && let Some(link) = self.links.get(executor)
    {
      return Ok(Target::Runner(link));
    }

    if !executor.is_empty()
      && executor != "local"
      && let Ok(instance) = self.pool.named(executor).await
      && let Some(runner) = instance.on_runner()
    {
      let link = self.links.get(runner).ok_or_else(|| {
        ControlError::Unavailable(crate::link::not_connected(runner))
          .with_diagnostic("runner.not_connected", [("runner".into(), runner.into())])
      })?;
      return Ok(Target::Runner(link));
    }

    Ok(Target::Local)
  }

  async fn run_fs_session(
    self,
    run: Option<Arc<Run>>,
    cancel: CancellationToken,
    mut requests: FromRunner<SessionRequest>,
    out_tx: mpsc::Sender<SessionResponse>,
  ) {
    let first = tokio::select! {
      item = requests.next() => item,
      () = cancel.cancelled() => return,
    };
    let first = match first {
      Some(Ok(req)) => req,
      Some(Err(status)) => {
        tracing::debug!("fs session stream failed before mount: {}", status.message());
        return;
      }
      None => return,
    };

    let Some(session_request::Body::Mount(ref mount)) = first.body else {
      let _ = out_tx.send(error_response(first.id, errno::EINVAL, "first request on fs session must be FsMount")).await;
      return;
    };

    if let Some(run) = &run {
      run.touch_activity();
    }

    // TODO(fs-file-faults): file-domain chaos faults (`FaultDomain::File`,
    // `file.open` interceptors) match by actor path, while `FsService`
    // `FsOpen` requests identify files by inode number (`ino`). Enforcing
    // `file.open` faults on `FsOpen`/`FsCreate`/`FsCreateAndWrite` requires
    // tracking `(parent, name) -> ino` paths across Lookup, ReadDirPlus,
    // Create, Rename and Forget; deferred to a follow-up slice once the FUSE
    // client lands.

    let target = match self.resolve_fs_target(run.as_ref(), &mount.executor).await {
      Ok(target) => target,
      Err(err) => {
        let _ = out_tx.send(error_response(first.id, errno::EIO, err.to_string())).await;
        return;
      }
    };

    match target {
      Target::Runner(link) => {
        let (runner_tx, mut runner_rx) = match link.open_fs().await {
          Ok(opened) => opened,
          Err(err) => {
            let _ = out_tx.send(error_response(first.id, errno::EIO, err.to_string())).await;
            return;
          }
        };
        let first_id = first.id;
        if runner_tx.send(first).await.is_err() {
          let _ = out_tx.send(error_response(first_id, errno::EIO, "runner fs task closed before mount")).await;
          return;
        }
        let done = CancellationToken::new();
        let req_pump = {
          let done = done.clone();
          let cancel = cancel.clone();
          let run = run.clone();
          async move {
            loop {
              let item = tokio::select! {
                item = requests.next() => item,
                () = done.cancelled() => break,
                () = cancel.cancelled() => break,
              };
              match item {
                Some(Ok(req)) => {
                  if let Some(run) = &run {
                    run.touch_activity();
                  }
                  if runner_tx.send(req).await.is_err() {
                    break;
                  }
                }
                Some(Err(status)) => {
                  tracing::debug!("fs session inbound stream error: {}", status.message());
                  break;
                }
                None => break,
              }
            }
            drop(runner_tx);
          }
        };
        let resp_pump = {
          let done = done.clone();
          async move {
            loop {
              let item = tokio::select! {
                item = runner_rx.next() => item,
                () = cancel.cancelled() => break,
              };
              match item {
                Some(Ok(FsTaskRequest {
                  payload: Some(fs_task_request::Payload::Fs(resp)),
                })) => {
                  if out_tx.send(resp).await.is_err() {
                    break;
                  }
                }
                Some(Ok(FsTaskRequest {
                  payload: Some(fs_task_request::Payload::Error(err)),
                })) => {
                  tracing::warn!("runner fs task failed: {}", err.message);
                  break;
                }
                Some(Ok(_)) => {}
                Some(Err(status)) => {
                  tracing::debug!("runner fs task stream error: {}", status.message());
                  break;
                }
                None => break,
              }
            }
            done.cancel();
          }
        };
        tokio::join!(req_pump, resp_pump);
      }
      Target::Local => {
        let lock_mode = FsLockMode::try_from(mount.lock_mode).unwrap_or(FsLockMode::Local);
        let fs = match HostFs::open(&mount.source, mount.writable, lock_mode) {
          Ok(fs) => fs,
          Err(err) => {
            let _ = out_tx
              .send(SessionResponse {
                id: first.id,
                body: Some(session_response::Body::Error(err)),
              })
              .await;
            return;
          }
        };
        let incoming = Box::pin(tokio_stream::once(Ok::<_, tonic::Status>(first)).chain(requests));
        let _ = cancel.run_until_cancelled(HostFs::serve_stream(incoming, out_tx, move |_| Ok(fs))).await;
      }
    }
  }
}

fn error_response(id: u64, code: i32, message: impl Into<String>) -> SessionResponse {
  SessionResponse {
    id,
    body: Some(session_response::Body::Error(FsError {
      errno: code,
      message: message.into(),
    })),
  }
}

#[async_trait::async_trait]
impl Fs for Daemon {
  async fn session(&self, caller: Caller, requests: FromRunner<SessionRequest>) -> Result<ToRunner<SessionResponse>, ControlError> {
    if matches!(caller, Caller::Runner { .. }) {
      return Err(ControlError::role_denied("runners cannot open filesystem sessions"));
    }
    self.admit_work()?;

    let run = match &caller {
      Caller::Actor { run } => self.runs.exact(run),
      Caller::Operator | Caller::Client { .. } | Caller::Runner { .. } => None,
    };
    let (tasks, cancel) = match &run {
      Some(run) => (run.tasks.clone(), run.cancel.clone()),
      None => (self.tasks.clone(), self.outside_runs.clone()),
    };

    // NOTICE: return the outbound response stream immediately so the client's
    // `FsService.Session` RPC receives HTTP/2 response headers before sending
    // its initial `FsMount` request (avoiding a bidi-stream handshake deadlock).
    let (out_tx, out_rx) = mpsc::channel::<SessionResponse>(QUEUE);
    let daemon = self.clone();
    tasks.spawn(daemon.run_fs_session(run, cancel, requests, out_tx));
    Ok(Box::pin(ReceiverStream::new(out_rx)))
  }
}

#[cfg(test)]
#[path = "fs_test.rs"]
mod fs_test;
