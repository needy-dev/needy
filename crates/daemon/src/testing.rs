//! Helpers for the daemon's tests: an executor served in-process, and a
//! control daemon on a temporary state directory, driven through
//! `api-server`'s domain traits.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use needy_api_server::control::{Caller, NewRun};
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
  /// `unix:` and the executor's socket.
  pub endpoint: String,
  _dir: Option<TempDir>,
  _stop: DropGuard,
}

impl Executor {
  pub async fn start() -> Self {
    let dir = tempfile::tempdir().unwrap();
    let mut executor = Self::start_at(&dir.path().join("executor.sock")).await;
    executor._dir = Some(dir);
    executor
  }

  /// An executor on `socket`, in a directory the caller keeps.
  pub async fn start_at(socket: &Path) -> Self {
    let health = Health {
      instance_id: "test".into(),
      role: "executor",
    };
    let service = ExecutorService {
      host: Host::default(),
      name: "local".into(),
    };
    let bound = Server::new(health, Caller::Cli).process_service(service).bind(&[Listen::Unix(socket.to_path_buf())]).await.unwrap();
    let cancel = CancellationToken::new();
    tokio::spawn(bound.serve(cancel.clone()));
    let endpoint = format!("unix:{}", socket.display());
    let client = provider::connect(&endpoint).await.unwrap();
    Self {
      native: Arc::new(Native::new(client)),
      endpoint,
      _dir: None,
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
  daemon_with_heartbeat(Duration::from_secs(15)).await
}

/// A control daemon whose runners send a heartbeat every `interval`.
pub async fn daemon_with_heartbeat(interval: Duration) -> Started {
  let dir = tempfile::tempdir().unwrap();
  // A stand-in KVM device: a file the daemon can open for reading and
  // writing. A test removes it to stand for a host without KVM.
  let kvm = dir.path().join("kvm");
  std::fs::write(&kvm, "").unwrap();
  let daemon = Daemon::start(Config {
    state_dir: dir.path().join("state"),
    runtime_dir: dir.path().join("run"),
    actor_image: dir.path().join("image"),
    kvm,
    docker_image: "needy-executor:latest".into(),
    heartbeat_interval: interval,
  })
  .await
  .unwrap();
  Started { daemon, dir }
}

/// A control daemon serving its API on a socket in its directory, for
/// the CLI holding `token`, and a client without a token.
pub struct Served {
  /// Keeps the daemon and its directory.
  _started: Started,
  pub client: needy_api_client::Client,
  /// `unix:` and the socket.
  pub endpoint: String,
  _stop: DropGuard,
}

pub async fn serve(started: Started, token: Option<&str>) -> Served {
  let socket = started.dir.path().join("api.sock");
  let bound = started.daemon.server(token.map(str::to_string)).bind(&[Listen::Unix(socket.clone())]).await.unwrap();
  let cancel = CancellationToken::new();
  tokio::spawn(bound.serve(cancel.clone()));
  let endpoint = format!("unix:{}", socket.display());
  let client = needy_api_client::Client::connect(&endpoint).await.unwrap();
  Served {
    _started: started,
    client,
    endpoint,
    _stop: cancel.drop_guard(),
  }
}

/// Writes a stand-in actor image into `dir`: empty kernel and root file
/// system, and a `firecracker` that runs `script` with `sh`. `$3` is the
/// VM's `vm.json`, so `$(dirname "$3")` is the run's directory.
pub fn fake_image(dir: &Path, script: &str) {
  use std::os::unix::fs::PermissionsExt;
  std::fs::create_dir_all(dir).unwrap();
  std::fs::write(dir.join("vmlinux"), "").unwrap();
  std::fs::write(dir.join("rootfs.ext4"), "").unwrap();
  let firecracker = dir.join("firecracker");
  std::fs::write(&firecracker, format!("#!/bin/sh\n{script}\n")).unwrap();
  std::fs::set_permissions(&firecracker, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A stand-in Firecracker that runs until its run's actor exits through
/// [`exit_actor`], as a guest reboots after its init reported the driving
/// program's exit.
pub const REBOOTS_AFTER_EXIT: &str = r#"dir=$(dirname "$3"); touch "$dir/v.sock"; while [ ! -e "$dir/rebooted" ]; do sleep 0.02; done"#;

/// Reports that the driving program of run `id` exited with 0, as the
/// guest's init does, then lets its [`REBOOTS_AFTER_EXIT`] VM exit.
pub async fn exit_actor(started: &Started, id: &str) {
  use needy_api_server::control::Guest;
  let exit = needy_common::process::Exit {
    code: Some(0),
    signal: String::new(),
  };
  Guest::exit(&started.daemon, id, exit).await.unwrap();
  std::fs::write(started.dir.path().join("state/runs").join(id).join("rebooted"), "").unwrap();
}

/// Whether process `pid` exists.
pub fn running(pid: i64) -> bool {
  // Signal 0 checks that the process exists without signalling it.
  unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// A run request for `profile_yaml` with an actor, which starts from
/// the image `fake_image` wrote into the daemon's `image` directory.
pub fn new_actor_run(profile_yaml: &str, dir: &Path) -> NewRun {
  NewRun {
    start_actor: true,
    ..new_run(profile_yaml, dir)
  }
}

/// A run request for `profile_yaml` without an actor, resolved as the
/// CLI does: relative paths against `dir`.
pub fn new_run(profile_yaml: &str, dir: &Path) -> NewRun {
  let resolved = needy_config::profile::resolve(&needy_config::Source::text(profile_yaml, dir)).unwrap();
  NewRun {
    profile_yaml: resolved.yaml,
    recipe_yaml: resolved.recipe_yaml.unwrap_or_default(),
    profile_name: "test".into(),
    start_actor: false,
    ..NewRun::default()
  }
}
