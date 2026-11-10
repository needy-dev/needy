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
    let endpoint = format!("unix:{}", socket.display());
    let client = provider::connect(&endpoint).await.unwrap();
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
  daemon_with(|_| {}).await
}

/// A control daemon whose runners send a heartbeat every `interval`.
pub async fn daemon_with_heartbeat(interval: Duration) -> Started {
  daemon_with(|config| config.heartbeat_interval = interval).await
}

/// A control daemon with its configuration changed by `adjust`. Its actors
/// start from a stand-in image whose VM runs until it is stopped
/// ([`PARKED`]); a test writes another with [`fake_image`].
pub async fn daemon_with(adjust: impl FnOnce(&mut Config)) -> Started {
  let dir = tempfile::tempdir().unwrap();
  // A stand-in KVM device: a file the daemon can open for reading and
  // writing. A test removes it to stand for a host without KVM.
  let kvm = dir.path().join("kvm");
  std::fs::write(&kvm, "").unwrap();
  fake_image(&dir.path().join("image"), PARKED);
  let mut config = Config {
    state_dir: dir.path().join("state"),
    runtime_dir: dir.path().join("run"),
    actor_image: dir.path().join("image"),
    kvm,
    docker_image: "needy-executor:latest".into(),
    heartbeat_interval: Duration::from_secs(15),
    local_runner: None,
  };
  adjust(&mut config);
  let daemon = Daemon::start(config).await.unwrap();
  Started { daemon, dir }
}

/// A stand-in Firecracker whose VM runs until its run stops it, or until
/// the test process that started the daemon exits, so no run of a test
/// leaves one behind. Its guest never boots: commands reach the run's
/// executors through `needy exec --run` only.
pub const PARKED: &str = r#"dir=$(dirname "$3"); touch "$dir/v.sock"; while kill -0 $PPID 2>/dev/null; do sleep 0.05; done"#;

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

impl Served {
  /// The daemon's temporary directory.
  pub fn dir(&self) -> &Path {
    self._started.dir.path()
  }

  pub fn daemon(&self) -> &Daemon {
    &self._started.daemon
  }
}

/// A runner joined to a served control plane and connected to it, serving
/// commands on this machine. It stops when dropped.
pub struct JoinedRunner {
  _state: TempDir,
  _stop: DropGuard,
}

/// Joins a runner `name` with `roles` and `labels` to `served` through
/// `operator` (a client holding the control token), starts it and waits
/// until its control stream is open and its first heartbeat reported its
/// host.
pub async fn join_runner(
  served: &Served,
  operator: &needy_api_client::Client,
  name: &str,
  roles: &[needy_core::types::RunnerRole],
  labels: &[(&str, &str)],
) -> JoinedRunner {
  let request = needy_api_client::proto::v1::CreateJoinTokenRequest {
    ttl_ms: 60_000,
    uses: 1,
    ..Default::default()
  };
  let token = operator.runners().create_join_token(request).await.unwrap().into_inner().token;
  let state = tempfile::tempdir().unwrap();
  let config = crate::runner::Config {
    state_dir: state.path().to_path_buf(),
    control: Some(served.endpoint.clone()),
    join_token: Some(token),
    name: name.into(),
    roles: roles.iter().copied().collect(),
    labels: labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
  };
  let runner = crate::runner::Runner::start(config).await.unwrap();
  let stop = CancellationToken::new();
  tokio::spawn(runner.serve(stop.clone()));
  let daemon = served.daemon();
  let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
  while daemon.links.get(name).is_none() || daemon.runner_host(name).is_none() {
    assert!(tokio::time::Instant::now() < deadline, "runner {name} did not connect");
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
  JoinedRunner {
    _state: state,
    _stop: stop.drop_guard(),
  }
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

/// Reports that the driving program of run `id` of the daemon whose
/// directory is `dir` exited with 0, as the guest's init does, then lets its
/// [`REBOOTS_AFTER_EXIT`] VM exit.
pub async fn exit_actor(daemon: &Daemon, dir: &Path, id: &str) {
  use needy_api_server::control::Guest;
  let exit = needy_common::process::Exit {
    code: Some(0),
    signal: String::new(),
  };
  Guest::exit(daemon, id, exit).await.unwrap();
  std::fs::write(dir.join("state/runs").join(id).join("rebooted"), "").unwrap();
}

/// Whether process `pid` exists.
pub fn running(pid: i64) -> bool {
  // Signal 0 checks that the process exists without signalling it.
  unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// A run request for `profile_yaml`, resolved as the CLI does: relative
/// paths against `dir`. Its actor starts from the daemon's stand-in image.
pub fn new_run(profile_yaml: &str, dir: &Path) -> NewRun {
  let resolved = needy_config::profile::resolve(&needy_config::Source::text(profile_yaml, dir)).unwrap();
  NewRun {
    profile_yaml: resolved.yaml,
    recipe_yaml: resolved.recipe_yaml.unwrap_or_default(),
    profile_name: "test".into(),
    ..NewRun::default()
  }
}
