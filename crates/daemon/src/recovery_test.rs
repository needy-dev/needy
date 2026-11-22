//! Recovery through Runs/Processes with a container-runtime boundary fixture.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt as _;
use needy_api_server::control::{Caller, Guest, GuestControl, Processes, Runs, StopMode};
use needy_api_server::{Health, Listen, Server};
use needy_core::types::{ActorState, Principal, RunState};
use needy_core_executor::driver::{LinuxContainer, Native, linux_container};
use needy_core_executor::engine::{self, Engine};
use needy_core_executor::process::Host;
use needy_core_executor::service::ExecutorService;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use crate::{Daemon, link, pool::Pool, run::Registry, testing};

type RunningService = (Host, CancellationToken, tokio::task::JoinHandle<()>);

/// A real process service stands in for each external container resource.
/// Removing that resource kills its processes and closes its actual socket.
#[derive(Default)]
struct Runtime {
  bodies: Mutex<HashMap<String, engine::models::ContainerCreateBody>>,
  sockets: Mutex<HashMap<String, PathBuf>>,
  running: Mutex<HashMap<String, RunningService>>,
  removal: Mutex<Option<Arc<Removal>>>,
}

/// Pause external removal before it starts or after its process service ends.
/// Both boundaries lie between durable release and final run persistence.
struct Removal {
  before: bool,
  removed: tokio::sync::Semaphore,
  acknowledge: tokio::sync::Semaphore,
}

impl Removal {
  fn new(before: bool) -> Arc<Self> {
    Arc::new(Self {
      before,
      removed: tokio::sync::Semaphore::new(0),
      acknowledge: tokio::sync::Semaphore::new(0),
    })
  }

  async fn pause(&self) {
    self.removed.add_permits(1);
    self.acknowledge.acquire().await.unwrap().forget();
  }
}

impl Drop for Runtime {
  fn drop(&mut self) {
    // A failing regression must still reclaim the processes this external
    // fixture owns. Proc::send delivers the signal synchronously.
    for (_, (host, cancel, served)) in self.running.get_mut().unwrap_or_else(|poisoned| poisoned.into_inner()).drain() {
      for process in host.list() {
        if process.exit().is_none() {
          process.send(needy_core_executor::process::Input::Signal("SIGKILL".into()));
        }
      }
      cancel.cancel();
      served.abort();
    }
  }
}

#[async_trait::async_trait]
impl Engine for Runtime {
  async fn runtimes(&self) -> Result<BTreeMap<String, Vec<String>>, engine::Error> {
    Ok(BTreeMap::new())
  }
  async fn inspect_image(&self, _: &str) -> Result<Option<engine::Image>, engine::Error> {
    Ok(Some(engine::Image::default()))
  }
  fn pull_progress(&self, _: &str) -> futures_util::stream::BoxStream<'static, Result<engine::models::CreateImageInfo, engine::Error>> {
    Box::pin(futures_util::stream::empty())
  }
  async fn create(&self, name: &str, body: engine::models::ContainerCreateBody) -> Result<(), engine::Error> {
    self.bodies.lock().unwrap().insert(name.into(), body);
    Ok(())
  }
  async fn start(&self, name: &str) -> Result<u32, engine::Error> {
    let body = self.bodies.lock().unwrap().get(name).unwrap().clone();
    let cmd = body.cmd.unwrap();
    let endpoint = cmd.windows(2).find(|pair| pair[0] == "--listen").unwrap()[1].strip_prefix("unix:").unwrap();
    let endpoint = std::path::Path::new(endpoint);
    let target = endpoint.parent().unwrap().to_str().unwrap();
    let binds = body.host_config.unwrap().binds.unwrap();
    let source = binds
      .iter()
      .find_map(|bind| {
        let (source, to) = bind.rsplit_once(':')?;
        (to == target).then_some(source)
      })
      .unwrap();
    let socket = PathBuf::from(source).join(endpoint.file_name().unwrap());
    self.sockets.lock().unwrap().insert(name.into(), socket.clone());
    let host = Host::default();
    let service = ExecutorService {
      host: host.clone(),
      name: name.into(),
    };
    let server = Server::new(
      Health {
        instance_id: name.into(),
        role: "executor",
      },
      Caller::Operator,
    )
    .process_service(service.preserving())
    .bind(&[Listen::Unix(socket)])
    .await
    .map_err(|e| engine::Error::Refused(e.to_string()))?;
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let served = tokio::spawn(async move {
      server.serve(stop).await.unwrap();
    });
    self.running.lock().unwrap().insert(name.into(), (host, cancel, served));
    Ok(std::process::id())
  }
  fn logs(&self, _: &str) -> futures_util::stream::BoxStream<'static, Result<bytes::Bytes, engine::Error>> {
    Box::pin(futures_util::stream::empty())
  }
  async fn wait(&self, _: &str) -> Result<i64, engine::Error> {
    Err(engine::Error::Refused("this fixture serves executor resources only".into()))
  }
  async fn stop(&self, name: &str, _: Duration) -> Result<(), engine::Error> {
    self.remove(name).await
  }
  async fn remove(&self, name: &str) -> Result<(), engine::Error> {
    let removal = self.removal.lock().unwrap().take();
    if let Some(removal) = removal.as_ref().filter(|removal| removal.before) {
      removal.pause().await;
    }
    self.bodies.lock().unwrap().remove(name);
    let running = self.running.lock().unwrap().remove(name);
    if let Some((host, cancel, served)) = running {
      host.shutdown().await;
      cancel.cancel();
      served.await.unwrap();
    }
    if let Some(removal) = removal.as_ref().filter(|removal| !removal.before) {
      removal.pause().await;
    }
    Ok(())
  }
  async fn download(&self, _: &str, _: &str) -> Result<Option<Vec<u8>>, engine::Error> {
    Ok(None)
  }
  async fn commit_container(&self, _: &str, _: &str, _: &str) -> Result<String, engine::Error> {
    Err(engine::Error::Refused("this fixture serves executor resources only".into()))
  }
  fn export_image(&self, _: &str) -> futures_util::stream::BoxStream<'static, Result<bytes::Bytes, engine::Error>> {
    Box::pin(futures_util::stream::empty())
  }
  async fn load_image(&self, _: futures_util::stream::BoxStream<'static, Result<bytes::Bytes, engine::Error>>) -> Result<(), engine::Error> {
    Err(engine::Error::Refused("this fixture serves executor resources only".into()))
  }
  async fn remove_image(&self, _: &str) -> Result<(), engine::Error> {
    Ok(())
  }
}

/// Compose replacement control around the same verified holder and durable
/// stores. Only the external container engine is substituted. Reconciliation
/// below is the same startup operation production control uses.
async fn control(before: &Daemon, runtime: Arc<Runtime>) -> Daemon {
  let endpoint = format!("unix:{}", before.config.runtime_dir.join("local.sock").display());
  let native = Arc::new(Native::new(needy_api_client::Client::connect(&endpoint).await.unwrap()));
  let container = LinuxContainer::with_engine(
    linux_container::Options {
      image: before.config.executor_image.clone(),
      state_dir: before.config.state_dir.clone(),
      allow: Vec::new(),
    },
    runtime,
  );
  let links: Arc<link::Links> = Arc::default();
  let cancel = CancellationToken::new();
  Daemon {
    pool: Arc::new(Pool::new(native, Some(container), links.clone())),
    runs: Arc::new(Registry::new(before.state.clone())),
    processes: Arc::default(),
    links,
    runner_hosts: Arc::default(),
    tasks: TaskTracker::new(),
    creates: TaskTracker::new(),
    outside_runs: cancel.child_token(),
    cancel,
    shutting_down: Arc::default(),
    suspending: Arc::default(),
    runs_stopped: Arc::default(),
    cleanup_incomplete: Arc::default(),
    shutdown_started: Arc::default(),
    ..before.clone()
  }
}

async fn accepted_stop(daemon: &Daemon, id: &str) {
  let mut controls = Guest::control(daemon, id).await.unwrap();
  let guest = tokio::spawn(async move {
    while let Some(control) = controls.next().await {
      if let GuestControl::Stop { .. } = control {
        return;
      }
    }
    panic!("the guest stream ended without accepting a stop");
  });
  // Polling the stream registers the actor's existing graceful-stop boundary.
  while !daemon.runs.find(id).unwrap().stop_request.waited_for() {
    tokio::task::yield_now().await;
  }
  let stopping = tokio::spawn({
    let (daemon, id) = (daemon.clone(), id.to_owned());
    async move {
      Runs::stop(
        &daemon,
        &id,
        Principal::Operator,
        StopMode::Graceful {
          grace: Duration::from_secs(60),
        },
      )
      .await
    }
  });
  guest.await.unwrap();
  assert_eq!(Runs::get(daemon, id).await.unwrap().state, RunState::Stopping);
  // A control crash loses this waiter after persisting the admitted stop.
  stopping.abort();
  assert!(stopping.await.unwrap_err().is_cancelled());
}

// ROOT CAUSE:
//
// If the first restored invocation was stopping, recovery released its shared
// container before a later live invocation reacquired its reference. The last
// release destroyed that live sibling's existing processes and service.
// Recovery must restore every reference before watches or stops can release it.
#[tokio::test]
async fn recovering_a_stopping_run_keeps_a_later_shared_container_run_alive() {
  // A short temporary root keeps the external executor socket below sun_path.
  let dir = tempfile::Builder::new().prefix("recovery-").tempdir_in("/tmp").unwrap();
  let started = testing::daemon_in(dir, |_| {}).await;
  let runtime = Arc::new(Runtime::default());
  let daemon = control(&started.daemon, runtime.clone()).await;
  let profile = format!(
    "actor: {{ driver: linux-firecracker }}\nuser: {{ name: user, uid: {}, gid: {} }}\nexecutors:\n  build: {{ driver: linux-container, key: shared }}\ncommands:\n  - {{ match: '**', to: build }}\n",
    nix_uid(),
    nix_gid()
  );
  let first = Runs::create(&daemon, testing::new_run(&profile, started.dir.path())).await.unwrap();
  tokio::time::sleep(Duration::from_millis(2)).await;
  let second = Runs::create(&daemon, testing::new_run(&profile, started.dir.path())).await.unwrap();
  assert!(first.created_ms < second.created_ms, "the stopping invocation must be restored first");
  let (process, pid) = detached_sleep(&daemon, &second.id, started.dir.path()).await;
  accepted_stop(&daemon, &first.id).await;
  daemon.suspend_control().await;
  let restarted = control(&daemon, runtime).await;

  let recovered = restarted.reconcile_open_runs().await;

  assert!(recovered.is_ok(), "recovery destroyed the later invocation's shared executor: {recovered:?}");
  assert!(testing::running(pid), "the surviving invocation's original process was destroyed");
  assert_eq!(Runs::get(&restarted, &first.id).await.unwrap().state, RunState::Stopped);
  assert_eq!(Runs::get(&restarted, &second.id).await.unwrap().state, RunState::Running);
  assert!(Processes::list(&restarted, &second.id, "").await.unwrap().iter().any(|p| p.process == process && p.running));
  Runs::stop(&restarted, &second.id, Principal::Operator, StopMode::Now).await.unwrap();
  assert!(testing::ended(pid).await);
  restarted.shutdown().await;
}

async fn detached_sleep(daemon: &Daemon, run: &str, cwd: &std::path::Path) -> (String, i64) {
  use needy_api_server::control::{ExecOpen, ExecSpec};
  let mut outputs = Processes::exec(
    daemon,
    ExecOpen::Start(ExecSpec {
      argv: vec!["sleep".into(), "300".into()],
      cwd: cwd.display().to_string(),
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
    }),
    Box::pin(futures_util::stream::pending()),
  )
  .await
  .unwrap();
  while let Some(output) = outputs.next().await {
    if let needy_api_server::control::ExecOutput::Started(process) = output.unwrap() {
      return (process.process, process.pid);
    }
  }
  panic!("the executor did not acknowledge its process");
}

fn nix_uid() -> u32 {
  // SAFETY: geteuid has no preconditions and does not mutate process state.
  unsafe { libc::geteuid() }
}
fn nix_gid() -> u32 {
  // SAFETY: getegid has no preconditions and does not mutate process state.
  unsafe { libc::getegid() }
}

// ROOT CAUSE:
//
// Recovery rewrote an already accepted stop as running before completing it.
// If replacement control failed again in that interval, its next startup no
// longer knew it must stop the invocation. A real backend bind failure marks
// the second interrupted startup without a production-only fault hook.
#[tokio::test]
async fn an_accepted_stop_survives_another_failed_control_start() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let profile = "actor: { driver: linux-firecracker }\nexecutors:\n  build: { driver: native, key: shared }\ncommands:\n  - { match: '**', to: build }\n";
  let first = Runs::create(daemon, testing::new_run(profile, started.dir.path())).await.unwrap();
  tokio::time::sleep(Duration::from_millis(2)).await;
  let second = Runs::create(daemon, testing::new_run(profile, started.dir.path())).await.unwrap();
  assert!(first.created_ms < second.created_ms, "the stopping invocation must activate before the bind fault");
  let (_, pid) = testing::detached_sleep(daemon, &second.id).await;
  accepted_stop(daemon, &first.id).await;
  daemon.suspend_control().await;
  let config = (*daemon.config).clone();

  // The first record activates, but the second cannot bind its real gateway
  // endpoint. Pending stop cleanup must not erase intent before this failure.
  let occupied = daemon.run_dir(&second.id).join("cg.sock");
  match std::fs::remove_file(&occupied) {
    Ok(()) => {}
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
    Err(error) => panic!("cannot install the filesystem bind fault: {error}"),
  }
  std::fs::create_dir(&occupied).unwrap();
  let failed = match Daemon::start(config.clone()).await {
    Ok(_) => panic!("the filesystem bind fault must interrupt startup"),
    Err(error) => error,
  };
  assert!(format!("{failed:#}").contains("cg.sock"), "{failed:#}");
  let survived_failed_start = testing::running(pid);
  std::fs::remove_dir(occupied).unwrap();

  // This also proves failed startup releases its discovery ownership and
  // guest/watch tasks while retaining the original protected work.
  let recovered = tokio::time::timeout(Duration::from_secs(10), Daemon::start(config)).await.unwrap().unwrap();
  let stopped = Runs::get(&recovered, &first.id).await.unwrap().state;
  let survivor = Runs::get(&recovered, &second.id).await.unwrap().state;
  let surviving_pid = testing::running(pid);
  recovered.shutdown().await;

  assert!(survived_failed_start, "an unsuccessful control restart ended unrelated held work");
  assert_eq!(stopped, RunState::Stopped, "the next startup forgot the accepted stop");
  assert_eq!(survivor, RunState::Running);
  assert!(surviving_pid);
  assert!(testing::ended(pid).await);
}

// A damaged snapshot makes only this invocation unadoptable. Its old shared
// executor record cannot authorize destroying the surviving sibling resource.
#[tokio::test]
async fn an_unadoptable_run_cannot_destroy_a_restored_siblings_shared_executor() {
  let dir = tempfile::Builder::new().prefix("recovery-").tempdir_in("/tmp").unwrap();
  let started = testing::daemon_in(dir, |_| {}).await;
  let runtime = Arc::new(Runtime::default());
  let daemon = control(&started.daemon, runtime.clone()).await;
  let profile = format!(
    "actor: {{ driver: linux-firecracker }}\nuser: {{ name: user, uid: {}, gid: {} }}\nexecutors:\n  build: {{ driver: linux-container, key: shared }}\ncommands:\n  - {{ match: '**', to: build }}\n",
    nix_uid(),
    nix_gid()
  );
  let first = Runs::create(&daemon, testing::new_run(&profile, started.dir.path())).await.unwrap();
  let second = Runs::create(&daemon, testing::new_run(&profile, started.dir.path())).await.unwrap();
  let (_, pid) = detached_sleep(&daemon, &second.id, started.dir.path()).await;
  daemon.suspend_control().await;
  std::fs::remove_file(daemon.run_dir(&first.id).join("control-snapshot.json")).unwrap();
  let recovered = control(&daemon, runtime).await;

  let reconciled = recovered.reconcile_open_runs().await;
  let alive = testing::running(pid);
  let surviving = Runs::get(&recovered, &second.id).await.unwrap().state;
  let lost = Runs::get(&recovered, &first.id).await.unwrap().state;
  recovered.shutdown().await;

  assert!(reconciled.is_ok(), "the lost sibling broke shared resource restoration: {reconciled:?}");
  assert!(alive, "cleanup of the unadoptable sibling destroyed the surviving process");
  assert_eq!(surviving, RunState::Running);
  assert_eq!(lost, RunState::Failed);
  assert!(testing::ended(pid).await);
}

// ROOT CAUSE:
//
// A stop persisted the released executor reference before deleting its external
// resource, but persisted the final stopped run afterwards. Recovery from that
// interval tried to reconnect the deleted container using the older snapshot.
// Accepted stop recovery must honor the newer durable release checkpoint.
#[tokio::test]
async fn an_interrupted_stop_does_not_restore_an_already_released_container() {
  interrupted_release(false, false).await;
}

// The same durable release checkpoint also precedes the external deletion.
// Retrying cleanup must remove that unheld resource without reacquiring it.
#[tokio::test]
async fn an_interrupted_stop_finishes_deleting_an_already_released_container() {
  interrupted_release(true, false).await;
}

// ROOT CAUSE:
//
// Recovery restored the snapshot's live actor state even after stop had
// durably released that actor. If stop completion ran before the restored
// watch observed the cached VM exit, it invented another forced stop outcome.
// Restore the release checkpoint before enabling the watch or resuming stop.
// CI regression: https://github.com/needy-dev/needy/actions/runs/43704794008
#[tokio::test]
async fn a_restored_stop_observes_actor_release_before_starting_its_watch() {
  interrupted_release(true, true).await;
}

async fn interrupted_release(before_removal: bool, before_watch: bool) {
  let dir = tempfile::Builder::new().prefix("recovery-").tempdir_in("/tmp").unwrap();
  let started = testing::daemon_in(dir, |_| {}).await;
  let runtime = Arc::new(Runtime::default());
  let daemon = control(&started.daemon, runtime.clone()).await;
  let profile = format!(
    "actor: {{ driver: linux-firecracker }}\nuser: {{ name: user, uid: {}, gid: {} }}\nexecutors:\n  build: {{ driver: linux-container, key: shared }}\ncommands:\n  - {{ match: '**', to: build }}\n",
    nix_uid(),
    nix_gid()
  );
  let run = Runs::create(&daemon, testing::new_run(&profile, started.dir.path())).await.unwrap();
  let (_, pid) = detached_sleep(&daemon, &run.id, started.dir.path()).await;
  let socket = runtime.sockets.lock().unwrap().values().next().unwrap().clone();
  let removed = Removal::new(before_removal);
  *runtime.removal.lock().unwrap() = Some(removed.clone());
  let stop = tokio::spawn({
    let (daemon, id) = (daemon.clone(), run.id.clone());
    async move { Runs::stop(&daemon, &id, Principal::Operator, StopMode::Now).await }
  });
  tokio::time::timeout(Duration::from_secs(5), removed.removed.acquire()).await.unwrap().unwrap().forget();
  assert_eq!(runtime.running.lock().unwrap().is_empty(), !before_removal);
  assert_eq!(socket.exists(), before_removal);
  stop.abort();
  assert!(stop.await.unwrap_err().is_cancelled());

  // Do not gracefully detach: that would replace the last snapshot and conceal
  // the crash window. Stop already drained the run's control-side tasks.
  let recovered = control(&daemon, runtime.clone()).await;
  let checkpoint = Runs::get(&recovered, &run.id).await.unwrap();
  assert_eq!(checkpoint.state, RunState::Stopping);
  assert!(checkpoint.executors.is_empty());
  assert_eq!(checkpoint.released.len(), 1);
  let reconciled = if before_watch {
    let instances = recovered.state.instances_of(&run.id).await.unwrap();
    let actor = instances.iter().find(|instance| instance.kind == crate::store::InstanceKind::Actor).unwrap();
    assert_eq!(actor.state, crate::store::InstanceState::Released);
    assert!(testing::ended(actor.pid.unwrap()).await, "the released actor must already be gone");
    let record = recovered.state.run(&run.id).await.unwrap().unwrap();
    let restored = recovered.restore_run(&record).await.unwrap().unwrap();
    // This is the startup boundary before any watch can repair stale state.
    // Assert through Runs rather than depending on which task runs first.
    assert_eq!(Runs::get(&recovered, &run.id).await.unwrap().actor, Some(ActorState::Exited));
    let stopping = restored.activate(&recovered).await.unwrap().unwrap();
    let actor = recovered.resume_stop(&stopping).await.unwrap();
    assert!(actor.is_none(), "an already released actor was stopped again: {actor:?}");
    Ok(())
  } else {
    recovered.reconcile_open_runs().await
  };
  let info = Runs::get(&recovered, &run.id).await.unwrap();
  let events = Runs::events(&recovered, &run.id).await.unwrap();
  let records = events
    .events
    .into_iter()
    .map(|event| needy_core::event::Recorded {
      time_ms: event.time_ms,
      event: needy_core::event::Event::decode(&event.kind, event.version, &event.payload).unwrap(),
    })
    .collect::<Vec<_>>();
  let report = needy_core::report::render(&run.id, &records, needy_core::report::Level::Summary, needy_core::report::Format::Json);
  let summary: serde_json::Value = serde_json::from_str(&report).unwrap();
  assert_eq!(summary["state"], "stopped", "report and durable stop disagree: {report}");
  assert!(summary["actor_stop"].is_null(), "recovery must not invent a new actor stop outcome: {report}");
  recovered.shutdown().await;

  assert!(reconciled.is_ok(), "an accepted stop reconnected an already released resource: {reconciled:?}");
  assert_eq!(info.state, RunState::Stopped);
  assert!(info.executors.is_empty());
  assert_eq!(info.released.len(), 1);
  assert_eq!(info.released[0].name, "build");
  assert!(runtime.running.lock().unwrap().is_empty(), "recovery left a released external resource alive");
  assert!(!socket.exists());
  assert!(testing::ended(pid).await);
}

// ROOT CAUSE:
//
// `restore_run` only restored `Boot::snapshot` from `state.db` and dropped all
// other `Boot` fields (`sync_files`, `start_hooks`, `stop_hooks`, `env`,
// `path`, `resolved_features`, `snapshot_image`, `commit_rebased`). After a
// control restart, feature-declared sync mappings and image metadata were lost.
//
// Before the fix, `restored.boot()` reverted to `Boot::default()` except for
// `snapshot`.
// The fix persists those `Boot` fields in `control-snapshot.json` with
// `#[serde(default)]` for backward compatibility and restores them on restart.
#[tokio::test]
async fn control_restart_preserves_boot_metadata_and_sync_files() {
  use needy_config::profile::OnConflict;
  use needy_core::image::FeatureSyncFile;

  let started = testing::daemon().await;
  let profile = "actor: { driver: linux-firecracker }\nexecutors:\n  here: { driver: native }\ncommands:\n  - { match: '**', to: here }\n";
  let created = Runs::create(&started.daemon, testing::new_run(profile, started.dir.path())).await.unwrap();
  let expected_boot = crate::run::Boot {
    snapshot: None,
    layer_disks: Vec::new(),
    snapshot_image: Some("needy-snapshot:deadbeef".into()),
    path: "/opt/tools/bin:/usr/bin:/bin".into(),
    env: BTreeMap::from([
      ("EDITOR".into(), "nvim".into()),
      ("LANG".into(), "C.UTF-8".into()),
    ]),
    session: Default::default(),
    commit_rebased: true,
    resolved_features: BTreeMap::from([("ghcr.io/example/feat:1".into(), "sha256:abcd".into())]),
    start_hooks: vec!["./start.sh".into()],
    stop_hooks: vec!["./stop.sh".into()],
    sync_files: vec![FeatureSyncFile {
      name: "tool-state".into(),
      store: "tool-state".into(),
      path: "/home/agent/.tool".into(),
      filter: vec!["+ config.json".into(), "- *".into()],
      on_conflict: vec![
        ("config.json".into(), OnConflict::Store),
        ("history.log".into(), OnConflict::Lines),
        ("cache.bin".into(), OnConflict::Run),
        ("state.db".into(), OnConflict::Driver("merge-sqlite".into())),
      ],
      feature: "ghcr.io/example/feat:1".into(),
    }],
  };
  started.daemon.runs.find(&created.id).unwrap().set_boot(expected_boot.clone());

  let started = testing::restart(started).await;
  let restored_boot = started.daemon.runs.find(&created.id).unwrap().boot();
  let info = Runs::get(&started.daemon, &created.id).await.unwrap();
  let guest = Guest::config(&started.daemon, &created.id).await.unwrap();

  assert_eq!(restored_boot, expected_boot);
  assert!(info.commit_rebased);
  assert_eq!(info.resolved_features, expected_boot.resolved_features);
  assert_eq!(guest.image_path, expected_boot.path);
  assert_eq!(guest.env.get("EDITOR").map(String::as_str), Some("nvim"));
  assert_eq!(guest.start_hooks, expected_boot.start_hooks);
  assert_eq!(guest.stop_hooks, expected_boot.stop_hooks);
  Runs::stop(&started.daemon, &created.id, Principal::Operator, StopMode::Now).await.unwrap();
  started.daemon.shutdown().await;
}

#[tokio::test]
async fn control_restart_reads_legacy_snapshot_without_boot_field() {
  let started = testing::daemon().await;
  let profile = "actor: { driver: linux-firecracker }\nexecutors:\n  here: { driver: native }\ncommands:\n  - { match: '**', to: here }\n";
  let created = Runs::create(&started.daemon, testing::new_run(profile, started.dir.path())).await.unwrap();
  let snapshot_path = started.daemon.run_dir(&created.id).join("control-snapshot.json");
  let config = (*started.daemon.config).clone();
  started.daemon.suspend_control().await;

  let mut raw: serde_json::Value = serde_json::from_slice(&std::fs::read(&snapshot_path).unwrap()).unwrap();
  raw.as_object_mut().unwrap().remove("boot");
  std::fs::write(&snapshot_path, serde_json::to_vec(&raw).unwrap()).unwrap();

  let restarted = Daemon::start(config).await.unwrap();
  assert_eq!(restarted.runs.find(&created.id).unwrap().boot(), crate::run::Boot::default());
  Runs::stop(&restarted, &created.id, Principal::Operator, StopMode::Now).await.unwrap();
  restarted.shutdown().await;
}
