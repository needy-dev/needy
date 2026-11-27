//! Workspace filesystem sessions (`FsService`) on the control daemon.
//!
//! Routes each `FsService.Session` stream to a connected remote runner (over
//! `RunnerService.FsTask`), a `linux-container` executor's `FsService` (over
//! its executor socket), or the local host filesystem (`HostFs`), based on the
//! executor named in the initial `FsMount` request.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{Stream, StreamExt as _};
use needy_api_client::proto::v1::{
  FsError, FsForgetItem, FsLockMode, FsTaskRequest, SessionRequest, SessionResponse, fs_task_request, session_request, session_response,
};
use needy_api_server::control::{Caller, ControlError, FromRunner, Fs, ToRunner};
use needy_core::chaos::Subject;
use needy_core::event::Event;
use needy_core_executor::fs::{HostFs, ROOT_INO, errno, linux_flags};
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
  Executor(Arc<dyn needy_api_client::Executor>),
}

/// Maps a POSIX errno name from a `file` chaos fault to the Linux wire errno
/// carried by `FsError`. Unknown names fall back to `EIO`.
fn fault_errno(name: &str) -> i32 {
  match name {
    "EPERM" => errno::EPERM,
    "ENOENT" => errno::ENOENT,
    "EIO" => errno::EIO,
    "EAGAIN" => errno::EAGAIN,
    "ENOMEM" => errno::ENOMEM,
    "EACCES" => errno::EACCES,
    "EBUSY" => errno::EBUSY,
    "EEXIST" => errno::EEXIST,
    "ENOTDIR" => errno::ENOTDIR,
    "EISDIR" => errno::EISDIR,
    "EINVAL" => errno::EINVAL,
    "EMFILE" => errno::EMFILE,
    "EFBIG" => errno::EFBIG,
    "ENOSPC" => errno::ENOSPC,
    "EROFS" => errno::EROFS,
    "EPIPE" => errno::EPIPE,
    "ENAMETOOLONG" => errno::ENAMETOOLONG,
    "ENOTEMPTY" => errno::ENOTEMPTY,
    "ELOOP" => errno::ELOOP,
    "EOPNOTSUPP" => errno::EOPNOTSUPP,
    "ESTALE" => errno::ESTALE,
    _ => errno::EIO,
  }
}

fn join_rel(parent: &str, name: &str) -> String {
  if parent.is_empty() {
    name.to_string()
  } else {
    format!("{parent}/{name}")
  }
}

struct InodeEntry {
  rel_path: String,
  nlookup: u64,
}

enum PendingOp {
  Mount,
  Child {
    parent: u64,
    name: String,
  },
  ReadDirPlus {
    dir_ino: u64,
  },
  Rename {
    parent: u64,
    name: String,
    new_parent: u64,
    new_name: String,
    exchange: bool,
  },
}

/// Tracks `ino -> actor_relative_path` across a single `FsService` session so
/// `file.open` chaos faults can match by the path the actor sees.
struct PathTable {
  target: String,
  root_ino: u64,
  inodes: HashMap<u64, InodeEntry>,
  pending: HashMap<u64, PendingOp>,
}

impl PathTable {
  fn new(target: &str) -> Self {
    let mut inodes = HashMap::new();
    inodes.insert(
      ROOT_INO,
      InodeEntry {
        rel_path: String::new(),
        nlookup: 1,
      },
    );
    Self {
      target: target.trim_end_matches('/').to_string(),
      root_ino: ROOT_INO,
      inodes,
      pending: HashMap::new(),
    }
  }

  fn actor_path(&self, rel: &str) -> String {
    if rel.is_empty() {
      if self.target.is_empty() {
        "/".to_string()
      } else {
        self.target.clone()
      }
    } else {
      format!("{}/{rel}", self.target)
    }
  }

  fn open_path(&self, body: &session_request::Body) -> Option<String> {
    match body {
      session_request::Body::Open(r) => self.inodes.get(&r.ino).map(|e| self.actor_path(&e.rel_path)),
      session_request::Body::Create(r) => self.inodes.get(&r.parent).map(|e| self.actor_path(&join_rel(&e.rel_path, &r.name))),
      session_request::Body::CreateAndWrite(r) => self.inodes.get(&r.parent).map(|e| self.actor_path(&join_rel(&e.rel_path, &r.name))),
      _ => None,
    }
  }

  fn record_root(&mut self, root_ino: u64) {
    self.root_ino = root_ino;
    self.inodes.insert(
      root_ino,
      InodeEntry {
        rel_path: String::new(),
        nlookup: 1,
      },
    );
  }

  fn record_inode(&mut self, ino: u64, rel_path: String) {
    self
      .inodes
      .entry(ino)
      .and_modify(|entry| {
        entry.rel_path = rel_path.clone();
        entry.nlookup = entry.nlookup.saturating_add(1);
      })
      .or_insert(InodeEntry {
        rel_path,
        nlookup: 1,
      });
  }

  fn forget(&mut self, items: &[FsForgetItem]) {
    for item in items {
      if item.ino == self.root_ino {
        continue;
      }
      if let Some(entry) = self.inodes.get_mut(&item.ino) {
        entry.nlookup = entry.nlookup.saturating_sub(item.nlookup);
        if entry.nlookup == 0 {
          self.inodes.remove(&item.ino);
        }
      }
    }
  }

  fn apply_rename(&mut self, old_rel: &str, new_rel: &str, exchange: bool) {
    let old_prefix = format!("{old_rel}/");
    let new_prefix = format!("{new_rel}/");
    for entry in self.inodes.values_mut() {
      if entry.rel_path == old_rel {
        entry.rel_path = new_rel.to_string();
      } else if let Some(rest) = entry.rel_path.strip_prefix(&old_prefix) {
        entry.rel_path = format!("{new_rel}/{rest}");
      } else if exchange {
        if entry.rel_path == new_rel {
          entry.rel_path = old_rel.to_string();
        } else if let Some(rest) = entry.rel_path.strip_prefix(&new_prefix) {
          entry.rel_path = format!("{old_rel}/{rest}");
        }
      }
    }
  }

  fn track_request(&mut self, req: &SessionRequest) {
    let Some(body) = &req.body else {
      return;
    };
    match body {
      session_request::Body::Mount(_) => {
        self.pending.insert(req.id, PendingOp::Mount);
      }
      session_request::Body::Lookup(r) => {
        self.pending.insert(
          req.id,
          PendingOp::Child {
            parent: r.parent,
            name: r.name.clone(),
          },
        );
      }
      session_request::Body::Create(r) => {
        self.pending.insert(
          req.id,
          PendingOp::Child {
            parent: r.parent,
            name: r.name.clone(),
          },
        );
      }
      session_request::Body::CreateAndWrite(r) => {
        self.pending.insert(
          req.id,
          PendingOp::Child {
            parent: r.parent,
            name: r.name.clone(),
          },
        );
      }
      session_request::Body::Mkdir(r) => {
        self.pending.insert(
          req.id,
          PendingOp::Child {
            parent: r.parent,
            name: r.name.clone(),
          },
        );
      }
      session_request::Body::Symlink(r) => {
        self.pending.insert(
          req.id,
          PendingOp::Child {
            parent: r.parent,
            name: r.name.clone(),
          },
        );
      }
      session_request::Body::Link(r) => {
        self.pending.insert(
          req.id,
          PendingOp::Child {
            parent: r.new_parent,
            name: r.new_name.clone(),
          },
        );
      }
      session_request::Body::Readdirplus(r) => {
        self.pending.insert(req.id, PendingOp::ReadDirPlus { dir_ino: r.ino });
      }
      session_request::Body::Rename(r) => {
        self.pending.insert(
          req.id,
          PendingOp::Rename {
            parent: r.parent,
            name: r.name.clone(),
            new_parent: r.new_parent,
            new_name: r.new_name.clone(),
            exchange: (r.flags & linux_flags::RENAME_EXCHANGE) != 0,
          },
        );
      }
      session_request::Body::Forget(r) => {
        self.forget(&r.items);
      }
      _ => {}
    }
  }

  fn on_response(&mut self, resp: &SessionResponse) {
    let Some(op) = self.pending.remove(&resp.id) else {
      return;
    };
    let Some(body) = &resp.body else {
      return;
    };
    match (op, body) {
      (PendingOp::Mount, session_response::Body::Mounted(m)) => {
        let root_ino = m.root.as_ref().map_or(ROOT_INO, |a| a.ino);
        self.record_root(root_ino);
      }
      (PendingOp::Child { parent, name }, session_response::Body::Entry(e)) => {
        if let Some(attr) = &e.attr
          && let Some(parent_rel) = self.inodes.get(&parent).map(|p| p.rel_path.clone())
        {
          self.record_inode(attr.ino, join_rel(&parent_rel, &name));
        }
      }
      (PendingOp::Child { parent, name }, session_response::Body::Created(c)) => {
        if let Some(attr) = &c.attr
          && let Some(parent_rel) = self.inodes.get(&parent).map(|p| p.rel_path.clone())
        {
          self.record_inode(attr.ino, join_rel(&parent_rel, &name));
        }
      }
      (PendingOp::ReadDirPlus { dir_ino }, session_response::Body::Readdirplus(r)) => {
        if let Some(dir_rel) = self.inodes.get(&dir_ino).map(|p| p.rel_path.clone()) {
          for entry in &r.entries {
            if entry.name == "." || entry.name == ".." {
              continue;
            }
            self.record_inode(entry.ino, join_rel(&dir_rel, &entry.name));
          }
        }
      }
      (
        PendingOp::Rename {
          parent,
          name,
          new_parent,
          new_name,
          exchange,
        },
        session_response::Body::Ok(_),
      ) => {
        let old_rel = self.inodes.get(&parent).map(|p| join_rel(&p.rel_path, &name));
        let new_rel = self.inodes.get(&new_parent).map(|p| join_rel(&p.rel_path, &new_name));
        if let (Some(old_rel), Some(new_rel)) = (old_rel, new_rel) {
          self.apply_rename(&old_rel, &new_rel, exchange);
        }
      }
      _ => {}
    }
  }
}

enum Intercept {
  Forward,
  Delay(Duration),
  Error {
    errno: i32,
    message: String,
    delay: Duration,
  },
}

/// Evaluates `file.open` chaos faults on a run-bound `FsService` session and
/// emits `fault.injected` trace events when a fault fires.
// NOTICE: installed whether or not the run has file faults at mount time,
// because `needy chaos add` may add one while the mount is open.
struct FsFaultFilter {
  run: Arc<Run>,
  paths: Mutex<PathTable>,
}

impl FsFaultFilter {
  fn new(run: Arc<Run>, target: &str) -> Self {
    Self {
      run,
      paths: Mutex::new(PathTable::new(target)),
    }
  }

  fn on_request(&self, req: &SessionRequest) -> Intercept {
    let open_path = {
      let paths = needy_common::sync::lock(&self.paths);
      req.body.as_ref().and_then(|body| paths.open_path(body))
    };

    if let Some(path) = open_path
      && let Some(fired) = self.run.injector.check(Subject::FileOpen(&path))
    {
      let delay = fired.effect.delay.unwrap_or(Duration::ZERO);
      let errno_name = fired.effect.errno.clone();
      self.run.emit(&Event::FaultInjected(fired));
      if let Some(errno_name) = errno_name {
        return Intercept::Error {
          errno: fault_errno(&errno_name),
          message: format!("needy chaos: {errno_name}"),
          delay,
        };
      }
      needy_common::sync::lock(&self.paths).track_request(req);
      if !delay.is_zero() {
        return Intercept::Delay(delay);
      }
      return Intercept::Forward;
    }

    needy_common::sync::lock(&self.paths).track_request(req);
    Intercept::Forward
  }

  fn on_response(&self, resp: &SessionResponse) {
    needy_common::sync::lock(&self.paths).on_response(resp);
  }
}

impl Daemon {
  /// Maps a resolved executor instance to its filesystem session target.
  fn target_for_instance(&self, instance: &crate::pool::Instance) -> Result<Target, ControlError> {
    if let Some(runner) = instance.on_runner() {
      let link = self.links.get(runner).ok_or_else(|| {
        ControlError::Unavailable(crate::link::not_connected(runner))
          .with_diagnostic("runner.not_connected", [("runner".into(), runner.into())])
      })?;
      return Ok(Target::Runner(link));
    }
    if instance.handle.driver == "linux-container" {
      return Ok(Target::Executor(instance.handle.executor.clone()));
    }
    Ok(Target::Local)
  }

  /// Resolves the executor named in `FsMount` to a connected remote runner, a
  /// `linux-container` executor service, or the local host filesystem.
  async fn resolve_fs_target(&self, run: Option<&Arc<Run>>, executor: &str) -> Result<Target, ControlError> {
    if let Some(run) = run
      && let Some(instance) = run.executor(executor)
    {
      return self.target_for_instance(&instance);
    }

    if !executor.is_empty()
      && let Some(link) = self.links.get(executor)
    {
      return Ok(Target::Runner(link));
    }

    if !executor.is_empty()
      && executor != "local"
      && let Ok(instance) = self.pool.named(executor).await
    {
      return self.target_for_instance(&instance);
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

    let mount_target = if mount.target.is_empty() {
      mount.source.clone()
    } else {
      mount.target.clone()
    };
    let filter = run.as_ref().map(|run| Arc::new(FsFaultFilter::new(run.clone(), &mount_target)));

    let target = match self.resolve_fs_target(run.as_ref(), &mount.executor).await {
      Ok(target) => target,
      Err(err) => {
        let _ = out_tx.send(error_response(first.id, errno::EIO, err.to_string())).await;
        return;
      }
    };

    match target {
      Target::Runner(link) => {
        let (runner_tx, runner_rx) = match link.open_fs().await {
          Ok(opened) => opened,
          Err(err) => {
            let _ = out_tx.send(error_response(first.id, errno::EIO, err.to_string())).await;
            return;
          }
        };
        let responses = runner_rx.take_while(|item| {
          let keep = match item {
            Ok(FsTaskRequest {
              payload: Some(fs_task_request::Payload::Error(err)),
            }) => {
              tracing::warn!("runner fs task failed: {}", err.message);
              false
            }
            Ok(_) => true,
            Err(status) => {
              tracing::debug!("runner fs task stream error: {}", status.message());
              false
            }
          };
          std::future::ready(keep)
        });
        let responses = responses.filter_map(|item| {
          let resp = match item {
            Ok(FsTaskRequest {
              payload: Some(fs_task_request::Payload::Fs(resp)),
            }) => Some(resp),
            _ => None,
          };
          std::future::ready(resp)
        });
        pump_fs_session(first, requests, runner_tx, responses, out_tx, filter, run, cancel).await;
      }
      Target::Executor(executor) => {
        let (exec_tx, exec_rx) = match executor.fs_session().await {
          Ok(opened) => opened,
          Err(status) => {
            let _ = out_tx.send(error_response(first.id, errno::EIO, status.message())).await;
            return;
          }
        };
        let responses = exec_rx
          .take_while(|item| {
            let keep = match item {
              Ok(_) => true,
              Err(status) => {
                tracing::debug!("executor fs session stream error: {}", status.message());
                false
              }
            };
            std::future::ready(keep)
          })
          .filter_map(|item| std::future::ready(item.ok()));
        pump_fs_session(first, requests, exec_tx, responses, out_tx, filter, run, cancel).await;
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
        let (backend_tx, backend_rx) = mpsc::channel::<SessionRequest>(QUEUE);
        let (resp_tx, resp_rx) = mpsc::channel::<SessionResponse>(QUEUE);
        let incoming = ReceiverStream::new(backend_rx).map(Ok::<_, std::convert::Infallible>);
        let server = cancel.run_until_cancelled(HostFs::serve_stream(incoming, resp_tx, move |_| Ok(fs)));
        let pump = pump_fs_session(first, requests, backend_tx, ReceiverStream::new(resp_rx), out_tx, filter, run, cancel.clone());
        tokio::join!(server, pump);
      }
    }
  }
}

#[allow(clippy::too_many_arguments)]
async fn pump_fs_session<R>(
  first: SessionRequest,
  mut requests: FromRunner<SessionRequest>,
  backend_tx: mpsc::Sender<SessionRequest>,
  responses: R,
  out_tx: mpsc::Sender<SessionResponse>,
  filter: Option<Arc<FsFaultFilter>>,
  run: Option<Arc<Run>>,
  cancel: CancellationToken,
) where
  R: Stream<Item = SessionResponse> + Send,
{
  if let Some(filter) = &filter {
    filter.on_request(&first);
  }
  let first_id = first.id;
  if backend_tx.send(first).await.is_err() {
    let _ = out_tx.send(error_response(first_id, errno::EIO, "fs backend closed before mount")).await;
    return;
  }

  let done = CancellationToken::new();
  let req_pump = {
    let done = done.clone();
    let cancel = cancel.clone();
    let run = run.clone();
    let filter = filter.clone();
    let out_tx = out_tx.clone();
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
            let decision = match &filter {
              Some(filter) => filter.on_request(&req),
              None => Intercept::Forward,
            };
            match decision {
              Intercept::Forward => {
                if backend_tx.send(req).await.is_err() {
                  break;
                }
              }
              Intercept::Delay(delay) => {
                let backend_tx = backend_tx.clone();
                let done = done.clone();
                let cancel = cancel.clone();
                let delayed = async move {
                  tokio::select! {
                    () = tokio::time::sleep(delay) => {
                      let _ = backend_tx.send(req).await;
                    }
                    () = done.cancelled() => {}
                    () = cancel.cancelled() => {}
                  }
                };
                if let Some(run) = &run {
                  run.tasks.spawn(delayed);
                } else {
                  tokio::spawn(delayed);
                }
              }
              Intercept::Error {
                errno,
                message,
                delay,
              } => {
                let resp = error_response(req.id, errno, message);
                if delay.is_zero() {
                  if out_tx.send(resp).await.is_err() {
                    break;
                  }
                } else {
                  let out_tx = out_tx.clone();
                  let done = done.clone();
                  let cancel = cancel.clone();
                  let delayed = async move {
                    tokio::select! {
                      () = tokio::time::sleep(delay) => {
                        let _ = out_tx.send(resp).await;
                      }
                      () = done.cancelled() => {}
                      () = cancel.cancelled() => {}
                    }
                  };
                  if let Some(run) = &run {
                    run.tasks.spawn(delayed);
                  } else {
                    tokio::spawn(delayed);
                  }
                }
              }
            }
          }
          Some(Err(status)) => {
            tracing::debug!("fs session inbound stream error: {}", status.message());
            break;
          }
          None => break,
        }
      }
      drop(backend_tx);
    }
  };
  let resp_pump = {
    let done = done.clone();
    async move {
      let mut responses = std::pin::pin!(responses);
      loop {
        let item = tokio::select! {
          item = responses.next() => item,
          () = cancel.cancelled() => break,
        };
        match item {
          Some(resp) => {
            if let Some(filter) = &filter {
              filter.on_response(&resp);
            }
            if out_tx.send(resp).await.is_err() {
              break;
            }
          }
          None => break,
        }
      }
      done.cancel();
    }
  };
  tokio::join!(req_pump, resp_pump);
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
