//! Helpers for the daemon's tests: an executor served in-process, and a
//! control daemon on a temporary state directory, driven through
//! `api-server`'s domain traits.

use std::path::Path;
use std::sync::Arc;

use needy_api_server::control::{Caller, NewSession};
use needy_api_server::{Health, Listen, Server};
use needy_core_executor::process::Host;
use needy_core_executor::provider::{self, Native};
use needy_core_executor::service::ExecutorService;
use tempfile::TempDir;
use tokio_util::sync::{CancellationToken, DropGuard};

use crate::{Config, Daemon};

/// An executor on a socket in a temporary directory, and the `native`
/// provider over it. It stops when dropped.
pub struct Executor {
  pub native: Arc<Native>,
  _dir: TempDir,
  _stop: DropGuard,
}

impl Executor {
  pub async fn start() -> Self {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("executor.sock");
    let health = Health {
      instance_id: "test".into(),
      role: "executor",
    };
    let service = ExecutorService {
      host: Host::default(),
      name: "local".into(),
    };
    let bound = Server::new(health, Caller::Cli).process_service(service).bind(&[Listen::Unix(socket.clone())]).await.unwrap();
    let cancel = CancellationToken::new();
    tokio::spawn(bound.serve(cancel.clone()));
    let client = provider::connect(&format!("unix:{}", socket.display())).await.unwrap();
    Self {
      native: Arc::new(Native::new(client)),
      _dir: dir,
      _stop: cancel.drop_guard(),
    }
  }
}

/// A control daemon whose state is in `dir`, which the test keeps.
pub struct Started {
  pub daemon: Daemon,
  pub dir: TempDir,
}

pub async fn daemon() -> Started {
  let dir = tempfile::tempdir().unwrap();
  let daemon = Daemon::start(Config {
    state_dir: dir.path().join("state"),
    runtime_dir: dir.path().join("run"),
    actor_image: dir.path().join("image"),
    docker_image: "needy-executor:latest".into(),
  })
  .await
  .unwrap();
  Started { daemon, dir }
}

/// A session request for `profile_yaml` without an actor, whose relative
/// paths resolve against `dir`.
pub fn new_session(profile_yaml: &str, dir: &Path) -> NewSession {
  NewSession {
    profile_yaml: profile_yaml.into(),
    profile_dir: dir.display().to_string(),
    profile_name: "test".into(),
    start_actor: false,
    ..NewSession::default()
  }
}
