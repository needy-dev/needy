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
    let bound = Server::new(health, Caller::Operator).process_service(service).bind(&[Listen::Unix(socket.clone())]).await.unwrap();
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
  daemon_in(tempfile::tempdir().unwrap(), adjust).await
}

/// A control daemon as [`daemon_with`] makes it, in `dir`, whose `state`
/// directory a test may have written already.
pub async fn daemon_in(dir: TempDir, adjust: impl FnOnce(&mut Config)) -> Started {
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
    report_wait: crate::REPORT_WAIT,
  };
  adjust(&mut config);
  let daemon = Daemon::start(config).await.unwrap();
  Started { daemon, dir }
}

/// Shuts `started`'s daemon down, as a termination signal does, and starts
/// another on the same directories, as a restart does.
pub async fn restart(started: Started) -> Started {
  started.daemon.shutdown().await;
  let config = (*started.daemon.config).clone();
  drop(started.daemon);
  let daemon = Daemon::start(config).await.unwrap();
  Started {
    daemon,
    dir: started.dir,
  }
}

/// Starts a detached `sleep 300` in run `run` and returns its process ID
/// on the executor and its PID.
pub async fn detached_sleep(daemon: &Daemon, run: &str) -> (String, i64) {
  use needy_api_server::control::{ExecOpen, ExecOutput, ExecSpec, Processes};
  use tokio_stream::StreamExt as _;
  let spec = ExecSpec {
    argv: vec!["sleep".into(), "300".into()],
    cwd: String::new(),
    env: Default::default(),
    clear_env: false,
    tty: None,
    detach: true,
    run: run.into(),
    executor: String::new(),
    runner: String::new(),
    no_stdin: true,
    origin: needy_core::types::Origin::Cli,
    search_path_if_missing: false,
    by: None,
  };
  let mut outputs = daemon.exec(ExecOpen::Start(spec), Box::pin(tokio_stream::pending())).await.unwrap();
  loop {
    if let ExecOutput::Started(started) = outputs.next().await.unwrap().unwrap() {
      break (started.process, started.pid);
    }
  }
}

/// Starts a detached `sleep 300` on runner `runner`'s machine, outside any
/// run, and returns its process ID and its PID.
pub async fn detached_sleep_on_runner(daemon: &Daemon, runner: &str) -> (String, i64) {
  use needy_api_server::control::{ExecOpen, ExecOutput, ExecSpec, Processes};
  use tokio_stream::StreamExt as _;
  let spec = ExecSpec {
    argv: vec!["sleep".into(), "300".into()],
    cwd: String::new(),
    env: Default::default(),
    clear_env: false,
    tty: None,
    detach: true,
    run: String::new(),
    executor: String::new(),
    runner: runner.into(),
    no_stdin: true,
    origin: needy_core::types::Origin::Cli,
    search_path_if_missing: false,
    by: None,
  };
  let mut outputs = daemon.exec(ExecOpen::Start(spec), Box::pin(tokio_stream::pending())).await.unwrap();
  loop {
    if let ExecOutput::Started(started) = outputs.next().await.unwrap().unwrap() {
      break (started.process, started.pid);
    }
  }
}

/// Waits up to ten seconds for process `pid` to end.
pub async fn ended(pid: i64) -> bool {
  for _ in 0..200 {
    if !running(pid) {
      return true;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
  }
  false
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

/// Capabilities as `runner ls` writes them: `actor(firecracker)
/// executor(docker,native)`.
pub fn capabilities(text: &str) -> needy_core::types::Capabilities {
  use needy_core::types::{ActorRuntime, ExecutorProvider};
  let mut capabilities = needy_core::types::Capabilities::default();
  for role in text.split_whitespace() {
    let (role, kinds) = role.trim_end_matches(')').split_once('(').expect("ROLE(KIND,...)");
    for kind in kinds.split(',') {
      match (role, kind) {
        ("actor", "firecracker") => capabilities.actor.insert(ActorRuntime::Firecracker),
        ("actor", "container") => capabilities.actor.insert(ActorRuntime::Container),
        ("actor", "vz") => capabilities.actor.insert(ActorRuntime::Vz),
        ("executor", "native") => capabilities.executor.insert(ExecutorProvider::Native),
        ("executor", "docker") => capabilities.executor.insert(ExecutorProvider::Docker),
        other => panic!("unknown capability {other:?}"),
      };
    }
  }
  capabilities
}

/// Joins a runner `name` with `capabilities` (as [`capabilities`] reads
/// them) and `labels` to `served` through
/// `operator` (a client holding the control token), starts it and waits
/// until its control stream is open and its first heartbeat reported its
/// host.
pub async fn join_runner(
  served: &Served,
  operator: &needy_api_client::Client,
  name: &str,
  capabilities: &str,
  labels: &[(&str, &str)],
) -> JoinedRunner {
  let config = crate::runner::Config {
    state_dir: Default::default(),
    control: Some(served.endpoint.clone()),
    join_token: None,
    name: name.into(),
    capabilities: self::capabilities(capabilities),
    labels: labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
    grace: crate::runner::DISCONNECT_GRACE,
  };
  join_runner_with(served, operator, config).await
}

/// Joins a runner configured as `config` (its state directory and join
/// token are the helper's) and waits as [`join_runner`] does.
pub async fn join_runner_with(served: &Served, operator: &needy_api_client::Client, mut config: crate::runner::Config) -> JoinedRunner {
  let request = needy_api_client::proto::v1::CreateJoinTokenRequest {
    ttl_ms: 60_000,
    uses: 1,
    ..Default::default()
  };
  let token = operator.runners().create_join_token(request).await.unwrap().into_inner().token;
  let state = tempfile::tempdir().unwrap();
  config.state_dir = state.path().to_path_buf();
  config.join_token = Some(token);
  let name = config.name.clone();
  let runner = crate::runner::Runner::start(config).await.unwrap();
  let stop = CancellationToken::new();
  tokio::spawn(runner.serve(stop.clone()));
  let daemon = served.daemon();
  let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
  while daemon.links.get(&name).is_none() || daemon.runner_host(&name).is_none() {
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

/// A Unix socket that forwards every connection to `target`, until cut.
pub struct Proxy {
  pub endpoint: String,
  connections: Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
  /// Set while new connections are closed at once, as with a tunnel that
  /// is down.
  refusing: Arc<std::sync::atomic::AtomicBool>,
  _accept: tokio::task::JoinHandle<()>,
}

impl Proxy {
  pub fn start(dir: &Path, target: &str) -> Self {
    let socket = dir.join("proxy.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let target = target.strip_prefix("unix:").unwrap().to_string();
    let connections: Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>> = Default::default();
    let refusing: Arc<std::sync::atomic::AtomicBool> = Default::default();
    let (open, refused) = (connections.clone(), refusing.clone());
    let accept = tokio::spawn(async move {
      while let Ok((mut inbound, _)) = listener.accept().await {
        if refused.load(std::sync::atomic::Ordering::SeqCst) {
          continue;
        }
        let target = target.clone();
        let forward = tokio::spawn(async move {
          if let Ok(mut outbound) = tokio::net::UnixStream::connect(&target).await {
            // Either side closing ends the connection.
            let _copied = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
          }
        });
        open.lock().unwrap().push(forward);
      }
    });
    Self {
      endpoint: format!("unix:{}", socket.display()),
      connections,
      refusing,
      _accept: accept,
    }
  }

  /// Drops every open connection at once, as a dying tunnel does.
  pub fn cut(&self) {
    for connection in self.connections.lock().unwrap().drain(..) {
      connection.abort();
    }
  }

  /// Cuts every connection and closes new ones until [`Proxy::restore`].
  pub fn down(&self) {
    self.refusing.store(true, std::sync::atomic::Ordering::SeqCst);
    self.cut();
  }

  pub fn restore(&self) {
    self.refusing.store(false, std::sync::atomic::Ordering::SeqCst);
  }
}
