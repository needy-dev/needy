//! The protected local runtime owner. Control may disappear; this process
//! retains child handles, native processes and the actor-facing socket files.
//! It admits no runs and makes no scheduling or routing decisions.

mod native;
mod wire;

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use needy_api_server::control::Caller;
use needy_api_server::{Health, Listen, Server};
use needy_core_actor::driver::{self, Running, Start};
use needy_core_executor::process::Host;
use needy_core_executor::service::ExecutorService;
use serde::{Deserialize, Serialize};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, watch};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub(crate) use wire::{Actor, Info};
use wire::{Command, Reply, Request, Response};

/// A holder's identity is independent of control discovery. The private token
/// and a live authenticated answer, never this PID, establish ownership.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
  version: u32,
  instance: String,
  token: String,
  socket: PathBuf,
}

#[derive(Clone)]
pub(crate) struct Client {
  record: Record,
  lease: Arc<std::sync::Mutex<Option<UnixStream>>>,
}

/// Container runtime boundaries used by the holder when starting container
/// actors. Defaults to this machine's local engine and `container` CLI; tests
/// substitute fakes at these real external boundaries.
#[derive(Clone)]
pub(crate) struct ContainerBackends {
  pub engine: Arc<dyn needy_core_executor::engine::Engine>,
  pub apple_cli: Arc<dyn needy_core_actor::driver::macos_apple_container::Cli>,
}

impl Default for ContainerBackends {
  fn default() -> Self {
    Self {
      engine: needy_core_executor::engine::local(),
      apple_cli: needy_core_actor::driver::macos_apple_container::default_cli(),
    }
  }
}

impl Client {
  pub(crate) fn id(&self) -> &str {
    &self.record.instance
  }

  /// Reconnects to a verified holder, or starts a new one. A newly started
  /// holder has a different identity; run adoption rejects the old identity.
  pub(crate) async fn connect_with_backends(config: &crate::Config, backends: ContainerBackends) -> Result<Self> {
    needy_api_server::create_private_dir(&config.runtime_dir)?;
    check_directory(&config.runtime_dir)?;
    if let Ok(record) = read_record(&config.runtime_dir) {
      let client = Self {
        record,
        lease: Default::default(),
      };
      if client.call(Command::Ping).await.is_ok() {
        client.keep_control().await?;
        return Ok(client);
      }
    }
    // Serializes concurrent starts independently of the holder's lifetime lock.
    let starting = private_file(&config.runtime_dir.join("holder-start.lock"), true)?;
    starting.lock()?;
    if let Ok(record) = read_record(&config.runtime_dir) {
      let client = Self {
        record,
        lease: Default::default(),
      };
      if client.call(Command::Ping).await.is_ok() {
        client.keep_control().await?;
        return Ok(client);
      }
    }
    // An answering record is required to reuse a live owner. Never replace
    // its sockets merely because its private descriptor was damaged.
    let ownership = private_file(&config.runtime_dir.join("holder.lock"), true)?;
    ownership.try_lock().context("a live runtime holder could not be authenticated; retained work was left untouched")?;
    drop(ownership);
    // Validate the longest startup listener synchronously so an invalid
    // Unix socket path fails before launching a detached process.
    let probe = bind(&config.runtime_dir.join("local.sock"))?;
    drop(probe);
    remove(&config.runtime_dir.join("local.sock"));
    let launch = config.runtime_dir.join("holder-config.json");
    write_private(&launch, config)?;
    #[cfg(test)]
    {
      let config = config.clone();
      tokio::spawn(async move {
        if let Err(error) = serve_with_backends(config, backends).await {
          tracing::error!("runtime holder: {error:#}");
        }
      });
    }
    #[cfg(not(test))]
    {
      let _ = backends;
      let mut command = std::process::Command::new(&config.holder_executable);
      command.args(["daemon", "holder", "--config-file"]).arg(&launch);
      command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
      // A service manager's control process group and terminal must not own
      // this holder. `setsid` is async-signal-safe in the pre-exec child.
      use std::os::unix::process::CommandExt as _;
      unsafe {
        command.pre_exec(|| {
          if libc::setsid() == -1 {
            return Err(std::io::Error::last_os_error());
          }
          Ok(())
        });
      }
      let mut child = command.spawn().context("cannot start the local runtime holder")?;
      // Reap the holder child when its owned work eventually ends. This
      // thread owns no runtime state and control may exit before it does.
      std::thread::spawn(move || {
        if let Err(error) = child.wait() {
          tracing::warn!("cannot reap the local runtime holder: {error}");
        }
      });
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
      if let Ok(record) = read_record(&config.runtime_dir) {
        let client = Self {
          record,
          lease: Default::default(),
        };
        if client.call(Command::Ping).await.is_ok() {
          client.keep_control().await?;
          return Ok(client);
        }
      }
      if tokio::time::Instant::now() >= deadline {
        bail!("the local runtime holder did not answer on {}", config.runtime_dir.join("holder.sock").display());
      }
      tokio::time::sleep(Duration::from_millis(20)).await;
    }
  }

  async fn keep_control(&self) -> Result<()> {
    let mut stream = UnixStream::connect(&self.record.socket).await?;
    verify_peer(&stream)?;
    wire::write(
      &mut stream,
      &Request {
        version: 1,
        instance: self.record.instance.clone(),
        token: self.record.token.clone(),
        command: Command::Lease,
      },
    )
    .await?;
    *needy_common::sync::lock(&self.lease) = Some(stream);
    Ok(())
  }

  pub(crate) fn disconnect(&self) {
    needy_common::sync::lock(&self.lease).take();
  }

  async fn call(&self, command: Command) -> Result<Reply> {
    let mut stream = UnixStream::connect(&self.record.socket).await.context("the local runtime holder is unavailable")?;
    verify_peer(&stream)?;
    wire::write(
      &mut stream,
      &Request {
        version: 1,
        instance: self.record.instance.clone(),
        token: self.record.token.clone(),
        command,
      },
    )
    .await?;
    let response: Response = wire::read(&mut stream).await?;
    if response.instance != self.record.instance {
      bail!("the runtime holder's identity changed");
    }
    response.result.map_err(anyhow::Error::msg)
  }

  pub(crate) async fn start(&self, actor: Actor) -> Result<Box<dyn Running>> {
    let run = actor.run.clone();
    match self.call(Command::Start(Box::new(actor))).await? {
      Reply::Actor(info) => Ok(Box::new(Remote {
        client: self.clone(),
        run,
        info,
      })),
      _ => bail!("the runtime holder returned no actor identity"),
    }
  }

  pub(crate) async fn adopt(&self, run: &str, identity: &str) -> Result<Box<dyn Running>> {
    if identity != self.id() {
      bail!("the original runtime holder was lost");
    }
    match self.call(Command::Inspect(run.into())).await? {
      Reply::Actor(info) => Ok(Box::new(Remote {
        client: self.clone(),
        run: run.into(),
        info,
      })),
      _ => bail!("the runtime holder returned no actor identity"),
    }
  }

  pub(crate) async fn end_run(&self, run: &str) -> Result<()> {
    self.call(Command::EndRun(run.into())).await.map(|_| ())
  }

  pub(crate) async fn shutdown(&self) -> Result<()> {
    self.call(Command::Shutdown).await.map(|_| ())
  }
}

struct Remote {
  client: Client,
  run: String,
  info: Info,
}

#[async_trait::async_trait]
impl Running for Remote {
  fn pid(&self) -> u32 {
    self.info.pid
  }
  fn vcpus(&self) -> u32 {
    self.info.vcpus
  }
  fn mem_mib(&self) -> u32 {
    self.info.mem_mib
  }
  async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
    self.result(Command::Wait(self.run.clone())).await
  }
  async fn stop(&mut self, grace: Duration) -> std::io::Result<std::process::ExitStatus> {
    self
      .result(Command::Stop {
        run: self.run.clone(),
        grace,
      })
      .await
  }
}

impl Remote {
  async fn result(&self, command: Command) -> std::io::Result<std::process::ExitStatus> {
    use std::os::unix::process::ExitStatusExt as _;
    match self.client.call(command).await.map_err(std::io::Error::other)? {
      Reply::Exited(Ok(raw)) => Ok(std::process::ExitStatus::from_raw(raw)),
      Reply::Exited(Err(error)) => Err(std::io::Error::other(error)),
      _ => Err(std::io::Error::other("the runtime holder returned no exit result")),
    }
  }
}

struct Held {
  info: Info,
  exit: watch::Sender<Option<std::result::Result<i32, String>>>,
  stop: watch::Sender<Option<Duration>>,
  cancel: CancellationToken,
}

struct Owner {
  record: Record,
  actors: Mutex<HashMap<String, Arc<Held>>>,
  controls: AtomicUsize,
  host: Host,
  backends: ContainerBackends,
  tasks: TaskTracker,
  cancel: CancellationToken,
}

/// Serves the hidden holder entry point. Its file is private local state,
/// produced by control; it is never a profile or a management API input.
pub async fn serve_file(path: &Path) -> Result<()> {
  let config = serde_json::from_reader(private_file(path, false)?)?;
  serve_with_backends(config, ContainerBackends::default()).await
}

async fn serve_with_backends(config: crate::Config, backends: ContainerBackends) -> Result<()> {
  check_directory(&config.runtime_dir)?;
  let lock = private_file(&config.runtime_dir.join("holder.lock"), true)?;
  lock.try_lock().context("another runtime holder owns this directory")?;
  let record = Record {
    version: 1,
    instance: uuid::Uuid::new_v4().to_string(),
    token: uuid::Uuid::new_v4().to_string(),
    socket: config.runtime_dir.join("holder.sock"),
  };
  let socket = bind(&record.socket)?;
  let host = Host::default();
  let owner = Arc::new(Owner {
    record: record.clone(),
    actors: Default::default(),
    controls: AtomicUsize::new(0),
    host: host.clone(),
    backends,
    tasks: TaskTracker::new(),
    cancel: CancellationToken::new(),
  });
  let health = Health {
    instance_id: record.instance.clone(),
    role: "executor",
  };
  let service = ExecutorService {
    host: host.clone(),
    name: "local".into(),
  };
  let local = config.runtime_dir.join("local.sock");
  let server = Server::new(health, Caller::Operator).process_service(native::Service(service)).bind(&[Listen::Unix(local.clone())]).await?;
  let stop = owner.cancel.clone();
  owner.tasks.spawn(async move {
    if let Err(error) = server.serve(stop).await {
      tracing::error!("holder's native executor: {error:#}");
    }
  });
  let idle = owner.clone();
  owner.tasks.spawn(async move {
    // Give launch/lease establishment time to complete, then end only when no
    // control instance, actor or native process still needs this holder.
    loop {
      tokio::select! {
        () = idle.cancel.cancelled() => break,
        () = tokio::time::sleep(Duration::from_secs(1)) => {}
      }
      if idle.controls.load(Ordering::SeqCst) == 0
        && idle.actors.lock().await.values().all(|actor| actor.exit.borrow().is_some())
        && idle.host.list().iter().all(|process| process.exit().is_some())
      {
        idle.cancel.cancel();
        break;
      }
    }
  });
  write_private(&config.runtime_dir.join("holder.json"), &record)?;
  loop {
    let (mut stream, _) = tokio::select! {
      () = owner.cancel.cancelled() => break,
      accepted = socket.accept() => accepted?,
    };
    if verify_peer(&stream).is_err() {
      continue;
    }
    let owner = owner.clone();
    owner.clone().tasks.spawn(async move {
      let result = async {
        let request: Request = wire::read(&mut stream).await?;
        use subtle::ConstantTimeEq as _;
        if request.version != 1
          || request.instance != owner.record.instance
          || !bool::from(request.token.as_bytes().ct_eq(owner.record.token.as_bytes()))
        {
          bail!("the runtime holder's credential or identity does not match");
        }
        if matches!(request.command, Command::Lease) {
          owner.controls.fetch_add(1, Ordering::SeqCst);
          use tokio::io::AsyncReadExt as _;
          tokio::select! {
            () = owner.cancel.cancelled() => {}
            _ = stream.read_u8() => {}
          }
          owner.controls.fetch_sub(1, Ordering::SeqCst);
          return Ok(Ok(Reply::Ready));
        }
        Ok(owner.command(request.command).await)
      }
      .await;
      let response = Response {
        instance: owner.record.instance.clone(),
        result: result.and_then(|reply| reply).map_err(|error| format!("{error:#}")),
      };
      // A control process may have exited while a wait was pending.
      if let Err(error) = wire::write(&mut stream, &response).await {
        tracing::debug!("runtime holder caller left: {error}");
      }
    });
  }
  for actor in owner.actors.lock().await.values() {
    actor.stop.send_replace(Some(Duration::ZERO));
  }
  host.shutdown().await;
  owner.tasks.close();
  owner.tasks.wait().await;
  for path in [record.socket, config.runtime_dir.join("holder.json")] {
    remove(&path);
  }
  drop(lock);
  Ok(())
}

impl Owner {
  async fn command(self: &Arc<Self>, command: Command) -> Result<Reply> {
    match command {
      Command::Ping | Command::Lease => Ok(Reply::Ready),
      Command::EndRun(run) => {
        // Existing stop cleanup lists/signals these processes through the
        // process API; this tombstone closes late dispatch before that list.
        drop(self.host.end_run(&run));
        Ok(Reply::Ready)
      }
      Command::Shutdown => {
        self.host.shutdown().await;
        self.cancel.cancel();
        Ok(Reply::Ready)
      }
      Command::Start(actor) => self.start(*actor).await.map(Reply::Actor),
      Command::Inspect(run) => Ok(Reply::Actor(self.held(&run).await?.info.clone())),
      Command::Stop { run, grace } => {
        let held = self.held(&run).await?;
        held.stop.send_replace(Some(grace));
        Ok(Reply::Exited(Self::wait(&held).await))
      }
      Command::Wait(run) => {
        let held = self.held(&run).await?;
        Ok(Reply::Exited(Self::wait(&held).await))
      }
    }
  }

  async fn held(&self, run: &str) -> Result<Arc<Held>> {
    self.actors.lock().await.get(run).cloned().context("the original actor is not owned by this runtime holder")
  }

  async fn wait(held: &Held) -> std::result::Result<i32, String> {
    let mut exit = held.exit.subscribe();
    exit.wait_for(Option::is_some).await.map_err(|error| error.to_string())?.clone().expect("an observed exit is present")
  }

  async fn start(self: &Arc<Self>, actor: Actor) -> Result<Info> {
    let mut actors = self.actors.lock().await;
    if actors.contains_key(&actor.run) {
      bail!("this invocation already has an actor; it cannot be started again");
    }
    let profile = needy_config::profile::load(&needy_config::Source::text(actor.profile_yaml.clone(), PathBuf::new()))?;
    let driver = driver_with_backends(&actor.host, &profile.actor, &self.backends)?;
    let channels = driver.channels(&actor.dir);
    let cancel = self.cancel.child_token();
    let proxies = [
      (&channels.guest, actor.dir.join("cu.sock")),
      (&channels.gateway, actor.dir.join("cg.sock")),
    ];
    for (driver::Endpoint::Unix(path), target) in proxies {
      proxy(path, target, &self.tasks, &cancel, driver::facets(profile.actor.driver).transport == driver::Transport::Socket)?;
    }
    let mut running = match driver
      .start(&Start {
        run: &actor.run,
        dir: &actor.dir,
        recipe: actor.recipe.as_ref(),
        user: &profile.user,
        mounts: &actor.mounts,
        mounts_dir: &actor.mounts_dir,
        guest: &actor.guest,
        layer_disks: &actor.layer_disks,
        snapshot_image: actor.snapshot_image.as_deref(),
      })
      .await
    {
      Ok(running) => running,
      Err(error) => {
        cancel.cancel();
        return Err(error.into());
      }
    };
    let info = Info {
      pid: running.pid(),
      vcpus: running.vcpus(),
      mem_mib: running.mem_mib(),
    };
    let held = Arc::new(Held {
      info: info.clone(),
      exit: watch::channel(None).0,
      stop: watch::channel(None).0,
      cancel,
    });
    actors.insert(actor.run.clone(), held.clone());
    self.tasks.spawn(async move {
      use std::os::unix::process::ExitStatusExt as _;
      let mut stop = held.stop.subscribe();
      let result = tokio::select! {
        result = running.wait() => result,
        grace = async { stop.wait_for(Option::is_some).await.ok().and_then(|grace| *grace).unwrap_or_default() } => {
          running.stop(grace).await
        }
      };
      held.exit.send_replace(Some(result.map(|status| status.into_raw()).map_err(|error| error.to_string())));
      held.cancel.cancel();
    });
    Ok(info)
  }
}

/// Builds the actor driver for `actor` on `host`, injecting `backends` for the
/// container drivers.
pub(crate) fn driver_with_backends(
  host: &driver::Host,
  actor: &needy_config::profile::Actor,
  backends: &ContainerBackends,
) -> Result<Box<dyn driver::Driver>, driver::Error> {
  match actor.driver {
    needy_config::profile::ActorDriver::LinuxContainer => {
      Ok(Box::new(driver::linux_container::LinuxContainer::of_with_engine(actor, backends.engine.clone())?))
    }
    needy_config::profile::ActorDriver::MacosAppleContainer => {
      Ok(Box::new(driver::macos_apple_container::MacosAppleContainer::with_cli(actor.container_image(), backends.apple_cli.clone())))
    }
    _ => driver::new(host, actor),
  }
}

/// The holder keeps the driver-facing socket inode alive. A new control
/// instance binds only the backend socket. Existing streams fail when their
/// control peer disappears; they are never replayed onto another connection.
fn proxy(path: &Path, target: PathBuf, tasks: &TaskTracker, cancel: &CancellationToken, open: bool) -> Result<()> {
  let listener = bind(path)?;
  if open {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))?;
    if let Some(dir) = path.parent() {
      std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))?;
    }
  }
  let (tasks, stop) = (tasks.clone(), cancel.clone());
  tasks.clone().spawn(async move {
    loop {
      let (mut incoming, _) = tokio::select! {
        () = stop.cancelled() => break,
        accepted = listener.accept() => match accepted { Ok(accepted) => accepted, Err(error) => { tracing::warn!("actor channel: {error}"); break; } },
      };
      let target = target.clone();
      let stop = stop.clone();
      tasks.spawn(async move {
        // Only connection establishment waits through a control gap. Bytes
        // already handed to a previous control instance are never retried.
        let outgoing = async {
          loop {
            match UnixStream::connect(&target).await {
              Ok(stream) => break stream,
              Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
          }
        };
        tokio::select! {
          () = stop.cancelled() => {}
          mut outgoing = outgoing => {
            if let Err(error) = tokio::io::copy_bidirectional(&mut incoming, &mut outgoing).await {
              tracing::debug!("actor channel disconnected: {error}");
            }
          }
        }
      });
    }
  });
  Ok(())
}

fn bind(path: &Path) -> Result<UnixListener> {
  if let Some(dir) = path.parent() {
    needy_api_server::create_private_dir(dir)?;
  }
  remove(path);
  let listener = UnixListener::bind(path).with_context(|| format!("cannot bind {}", path.display()))?;
  std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
  Ok(listener)
}

fn remove(path: &Path) {
  if let Err(error) = std::fs::remove_file(path)
    && error.kind() != std::io::ErrorKind::NotFound
  {
    tracing::warn!("cannot remove {}: {error}", path.display());
  }
}

fn verify_peer(stream: &UnixStream) -> Result<()> {
  // SAFETY: geteuid has no preconditions and does not mutate process state.
  if stream.peer_cred()?.uid() != unsafe { libc::geteuid() } {
    bail!("runtime holder peer is not the invoking account");
  }
  Ok(())
}

fn check_directory(path: &Path) -> Result<()> {
  let metadata = std::fs::symlink_metadata(path)?;
  // SAFETY: geteuid has no preconditions and does not mutate process state.
  if !metadata.is_dir() || metadata.mode() & 0o077 != 0 || metadata.uid() != unsafe { libc::geteuid() } {
    bail!("runtime holder directory must be private and owned by the invoking account");
  }
  Ok(())
}

fn private_file(path: &Path, create: bool) -> Result<File> {
  let file = OpenOptions::new().read(true).write(create).create(create).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(path)?;
  let metadata = file.metadata()?;
  // SAFETY: geteuid has no preconditions and does not mutate process state.
  if !metadata.is_file() || metadata.nlink() != 1 || metadata.mode() & 0o077 != 0 || metadata.uid() != unsafe { libc::geteuid() } {
    bail!("runtime holder files must be private regular files owned by the invoking account");
  }
  Ok(file)
}

fn write_private(path: &Path, value: &impl Serialize) -> Result<()> {
  let mut bytes = serde_json::to_vec(value)?;
  bytes.push(b'\n');
  needy_common::fs::write_atomic(path, &bytes, Some(0o600))?;
  Ok(())
}

fn read_record(runtime: &Path) -> Result<Record> {
  check_directory(runtime)?;
  let record: Record = serde_json::from_reader(private_file(&runtime.join("holder.json"), false)?)?;
  if record.version != 1 || record.instance.is_empty() || record.token.is_empty() || record.socket != runtime.join("holder.sock") {
    bail!("unsupported or invalid runtime holder record");
  }
  Ok(record)
}
