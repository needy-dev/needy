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

/// Writes a stand-in actor image into `dir`: empty kernel and root file
/// system, and a `firecracker` that runs `script` with `sh`. `$3` is the
/// VM's `vm.json`, so `$(dirname "$3")` is the session's directory.
pub fn fake_image(dir: &Path, script: &str) {
  use std::os::unix::fs::PermissionsExt;
  std::fs::create_dir_all(dir).unwrap();
  std::fs::write(dir.join("vmlinux"), "").unwrap();
  std::fs::write(dir.join("rootfs.ext4"), "").unwrap();
  let firecracker = dir.join("firecracker");
  std::fs::write(&firecracker, format!("#!/bin/sh\n{script}\n")).unwrap();
  std::fs::set_permissions(&firecracker, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Whether process `pid` exists.
pub fn running(pid: i64) -> bool {
  // Signal 0 checks that the process exists without signalling it.
  unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// A session request for `profile_yaml` with an actor, which starts from
/// the image `fake_image` wrote into the daemon's `image` directory.
pub fn new_actor_session(profile_yaml: &str, dir: &Path) -> NewSession {
  NewSession {
    start_actor: true,
    ..new_session(profile_yaml, dir)
  }
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
