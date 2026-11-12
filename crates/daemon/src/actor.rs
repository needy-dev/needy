//! Wires a run to its actor: the guest listener the actor's init and
//! proxy call, the gateway its network goes through, and the actor its
//! profile's runtime starts.
//!
//! The actor's tasks belong to its run and run under a child of the
//! run's token, which the actor's exit also cancels: the guest listener and
//! the gateway stop when the actor exits or its run stops.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use needy_api_server::control::{Caller, ControlError};
use needy_api_server::{Health, Listen, Server};
use needy_config::profile::{ContainerActor, FaultKind, FileMode, Runtime, Side};
use needy_core::event::{ActorStart, ActorVmExit, Event};
use needy_core::types::{Effect, Fired, Network, RunState};
use needy_core_actor::host::container::Container;
use needy_core_actor::host::firecracker::{self, Firecracker};
use needy_core_actor::host::gateway;
use needy_core_actor::host::macos::apple_container::AppleContainer;
use needy_core_actor::host::macos::vz::{self, Vz};
use needy_core_actor::host::runtime::{self, ActorRuntime, Channels, Endpoint, HostMount, RunningActor, Setup, Start, Terminal};
use tokio_util::sync::CancellationToken;

use crate::Daemon;
use crate::pool::Instance;
use crate::run::Run;
use crate::store::{self, InstanceKind, InstanceRecord, InstanceState};

// NOTICE: the guest's state is not kept, so the actor gets a short time to
// exit on `SIGTERM` before it is killed; a run's stop waits this long at
// most for its actor.
const VM_STOP_GRACE: Duration = Duration::from_secs(3);

/// The runtime that runs this host's actors as `actor` (a profile's
/// `actor:`) says.
pub fn runtime(daemon: &Daemon, actor: &needy_config::profile::Actor) -> Result<Box<dyn ActorRuntime>> {
  match actor.runtime {
    Runtime::Firecracker => Ok(Box::new(Firecracker {
      image: firecracker::Image::new(&daemon.config.actor_image),
      kvm: daemon.config.kvm.clone(),
    })),
    Runtime::Container => {
      // `validate::profile` refuses a container actor without its options
      // before a create gets here.
      let Some(ContainerActor {
        oci_runtime: Some(oci_runtime),
        image,
      }) = &actor.container
      else {
        bail!("actor.container.oci_runtime is missing");
      };
      Ok(Box::new(Container::new(*oci_runtime, image.clone())))
    }
    Runtime::MacosVz => Ok(Box::new(Vz {
      image: vz::Image::new(&daemon.config.actor_image),
      vfkit: vz::VFKIT.into(),
    })),
    Runtime::MacosAppleContainer => Ok(Box::new(AppleContainer::new(ContainerActor::image_of(actor.container.as_ref())))),
  }
}

/// Where the daemon and `run`'s actor meet, as its runtime names them.
pub fn channels(daemon: &Daemon, run: &Run) -> Result<Channels> {
  Ok(runtime(daemon, &run.profile.actor)?.channels(&run.dir))
}

/// Who sets up `run`'s mounts and identity: its actor's init or its
/// runtime.
pub fn setup(daemon: &Daemon, run: &Run) -> Result<Setup> {
  Ok(runtime(daemon, &run.profile.actor)?.setup())
}

/// Fails unless this host can run `run`'s actor. A create checks this
/// before it acquires anything, so a host without KVM, or without the
/// container's OCI runtime or image, fails at once.
pub async fn check_host(daemon: &Daemon, run: &Run) -> Result<()> {
  let runtime = runtime(daemon, &run.profile.actor)?;
  runtime.check_host().await?;
  let mut required = Vec::new();
  for (is_error, message) in identity_findings(runtime.as_ref(), run.recipe.as_ref()) {
    if is_error {
      required.push(message);
    } else {
      tracing::info!("run {}: {message}", run.id);
    }
  }
  if !required.is_empty() {
    bail!("{}", required.join("; "));
  }
  Ok(())
}

/// What of `recipe` (or of a plain machine) `runtime` does not present, as
/// findings: an error, `true`, for a field the recipe requires, a warning
/// otherwise.
pub fn identity_findings(runtime: &dyn ActorRuntime, recipe: Option<&needy_core::recipe::CompiledRecipe>) -> Vec<(bool, String)> {
  runtime
    .identity_gaps(recipe)
    .into_iter()
    .map(|gap| match gap.field {
      Some(field) if recipe.is_some_and(|r| r.required.contains(&field)) => {
        (true, format!("the recipe requires {}, which a {} actor does not present: {}", field.as_str(), runtime.name(), gap.message))
      }
      Some(field) => (false, format!("{} ({}): {}", field.as_str(), runtime.name(), gap.message)),
      None => (false, format!("{}: {}", runtime.name(), gap.message)),
    })
    .collect()
}

/// The workspace mounts of `run`: executor directories mounted into the
/// actor over SFTP.
pub fn mounts(run: &Run) -> Vec<HostMount> {
  let mut mounts = Vec::new();
  for (name, mapping) in &run.profile.files.mappings {
    if !crate::files::selected(run, name) || mapping.mode != FileMode::Mount {
      continue;
    }
    let (Side::Executor { name: exec, path }, Side::Actor(target)) = (&mapping.from, &mapping.to) else {
      continue;
    };
    let Some(instance) = run.executor(exec) else {
      continue;
    };
    mounts.push(HostMount {
      target: target.clone(),
      executor: exec.clone(),
      source: path.clone(),
      writable: mapping.writable,
      sftp_command: instance.handle.sftp.clone(),
    });
  }
  mounts
}

/// Starts the actor of `run`: its guest listener, its gateway, then the
/// actor itself.
pub async fn start(daemon: &Daemon, run: &Arc<Run>) -> Result<()> {
  let runtime = runtime(daemon, &run.profile.actor)?;
  let channels = runtime.channels(&run.dir);
  let actor = run.cancel.child_token();
  if let Err(e) = serve_guest(daemon, run, &channels, &actor).await {
    actor.cancel();
    return Err(e);
  }
  if let Err(e) = serve_gateway(run, &channels, &actor) {
    actor.cancel();
    return Err(e);
  }
  // A runtime that sets the actor up from the host mounts the workspace
  // itself; a VM's init, or an Apple `container` actor's, does it inside.
  let mounts = if runtime.setup().mounts_in_guest() {
    Vec::new()
  } else {
    mounts(run)
  };
  let start = Start {
    run: &run.id,
    dir: &run.dir,
    recipe: run.recipe.as_ref(),
    user: &run.profile.user,
    mounts: &mounts,
  };
  let vm = match runtime.start(&start).await {
    Ok(vm) => vm,
    Err(e) => {
      actor.cancel();
      return Err(e.into());
    }
  };
  run.emit(&Event::ActorStart(ActorStart {
    pid: vm.pid(),
    vcpus: vm.vcpus(),
    mem_mib: vm.mem_mib(),
    runtime: Some(runtime.label()),
  }));
  // A restarted control plane finds the VM by its PID and directory.
  let record = InstanceRecord {
    id: store::actor_instance(&run.id),
    run: run.id.clone(),
    name: "actor".into(),
    kind: InstanceKind::Actor,
    // The control plane runs it, also when placed on its own runner.
    runner: None,
    provider: runtime.name().into(),
    runtime: runtime.name().into(),
    handle: run.dir.display().to_string(),
    pid: Some(i64::from(vm.pid())),
    state: InstanceState::Running,
    created_ms: needy_common::time::now_ms(),
    ended_ms: None,
  };
  daemon.save_instance(&record).await;
  let name = runtime.name();
  // Under `GuestMounts` (Apple's `container`) the actor has no sensors to
  // set; `profile check` says so, and the faults are not recorded as fired.
  if runtime.setup() == Setup::Host {
    schedule_hwmon_faults(run, Arc::from(runtime), &actor);
  }
  run.tasks.spawn(watch(daemon.clone(), run.clone(), name, vm, actor));
  Ok(())
}

/// Schedules the run's hwmon faults on an actor whose runtime presents its
/// sensors from the host: each sets the sensors at its time after the
/// actor started, and is recorded in the run's trace as a VM's init records
/// it.
fn schedule_hwmon_faults(run: &Arc<Run>, runtime: Arc<dyn ActorRuntime>, actor: &CancellationToken) {
  for fault in run.injector.faults().iter().filter(|f| f.on == FaultKind::Hwmon) {
    let at = Duration::from_millis(fault.at.map(|d| d.as_millis() as u64).unwrap_or(0));
    let temp_mc = fault.set.temp_c.map(|c| (c * 1000.0) as i32).unwrap_or(100_000);
    let (task_run, runtime, actor) = (run.clone(), runtime.clone(), actor.clone());
    run.tasks.spawn(async move {
      let run = task_run;
      tokio::select! {
        () = actor.cancelled() => return,
        () = tokio::time::sleep(at) => {}
      }
      if let Err(e) = runtime.overheat(&run.dir, temp_mc) {
        tracing::warn!("run {}: cannot apply the hwmon fault: {e}", run.id);
        return;
      }
      run.emit(&Event::FaultInjected(Fired {
        kind: FaultKind::Hwmon,
        target: "coretemp".into(),
        effect: Effect {
          temp_mc: Some(temp_mc),
          ..Effect::default()
        },
      }));
    });
  }
}

/// How long an attach waits for the actor's process service: the guest
/// serves it once it started the driving program, after boot and setup.
// NOTICE: setup includes workspace mounts and copy-in, which take seconds on
// a large workspace; a minute covers them, and an attach to a run whose
// actor never serves its terminal fails then.
const TERMINAL_WAIT: Duration = Duration::from_secs(60);

/// The process service of `run`'s actor for its driving program on a
/// terminal, reached the way its runtime says (the VM's vsock device under
/// Firecracker). Waits for the actor to serve it while the run runs.
pub async fn terminal(daemon: &Daemon, run: &Arc<Run>) -> Result<Arc<Instance>, ControlError> {
  let channels = channels(daemon, run).map_err(|e| ControlError::Failed(format!("run {}: {e:#}", run.id)))?;
  let deadline = tokio::time::Instant::now() + TERMINAL_WAIT;
  loop {
    type Connecting<'a> =
      std::pin::Pin<Box<dyn std::future::Future<Output = Result<needy_api_client::Client, needy_api_client::Error>> + Send + 'a>>;
    let connect: Connecting<'_> = match &channels.terminal {
      Terminal::FirecrackerVsock { device, port } => Box::pin(needy_api_client::Client::connect_firecracker_vsock(device, *port)),
      Terminal::Unix(path) => Box::pin(connect_terminal(path)),
    };
    // Bound the connection itself, including the Firecracker handshake:
    // a peer that accepts but never answers must not hold `run attach`.
    let connected = tokio::select! {
      () = run.cancel.cancelled() => return Err(ControlError::Unavailable(format!("run {} stopped while waiting for its terminal", run.id))),
      connected = tokio::time::timeout_at(deadline, connect) => {
        connected.map_err(|_| ControlError::Unavailable(format!("timed out waiting for the terminal of run {}", run.id)))?
      }
    };
    let error = match connected {
      Ok(client) => return Ok(Instance::actor(&run.id, Arc::new(client))),
      Err(e) => e,
    };
    if tokio::time::Instant::now() >= deadline || run.cancel.is_cancelled() {
      return Err(ControlError::Unavailable(format!("the actor of run {} does not serve its terminal: {error}", run.id)));
    }
    tracing::debug!("run {}: the actor's terminal is not served yet: {error}", run.id);
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
}

/// Connects to an actor's process service on the Unix socket `path`, once
/// the service itself answers.
// NOTICE: a relay can accept before the actor serves: vfkit's `connect`
// socket of a macos-vz actor accepts and only then dials the guest's vsock port,
// closing the connection when nothing listens there yet, so the first call
// failed with "transport error" (seen 2026-11-12 with `run create -it`). A
// health check waits for the service: the process service has no health
// service and answers `Unimplemented`; the closed relay fails the call with
// `Unknown` ("transport error"), not `Unavailable`. A socket Apple's
// `container` publishes (`--publish-socket`, an macos-apple-container actor's
// process socket) accepts on the host in the same way, seen the same day
// with `container` 1.7.1, see
// `docs/ai/references/architecture/2026-11-12-macos-apple-container-actor-evidence.md`.
async fn connect_terminal(path: &Path) -> Result<needy_api_client::Client, needy_api_client::Error> {
  let client = needy_api_client::Client::connect(&format!("unix:{}", path.display())).await?;
  match client.health().check(needy_api_client::proto::v1::CheckRequest {}).await {
    Ok(_) => Ok(client),
    Err(status) if status.code() == tonic::Code::Unimplemented => Ok(client),
    Err(status) => Err(needy_api_client::Error::Status(status)),
  }
}

/// The listener the API server binds for `endpoint`.
fn listen(endpoint: &Endpoint) -> Listen {
  match endpoint {
    Endpoint::Unix(path) => Listen::Unix(path.clone()),
  }
}

/// Serves the run's API to the actor on its guest channel, as
/// `Caller::Actor` bound to the run, until `actor` is cancelled.
async fn serve_guest(daemon: &Daemon, run: &Run, channels: &Channels, actor: &CancellationToken) -> Result<()> {
  let api = Arc::new(daemon.clone());
  let health = Health {
    instance_id: uuid::Uuid::new_v4().to_string(),
    role: "control",
  };
  let caller = Caller::Actor {
    run: run.id.clone(),
  };
  let listener = Server::new(health, caller).runs(api.clone()).processes(api.clone()).guest(api).bind(&[listen(&channels.guest)]).await?;
  if channels.guest_for_every_user {
    open_to_every_user(&channels.guest)?;
  }
  let id = run.id.clone();
  let stop = actor.clone();
  run.tasks.spawn(async move {
    if let Err(e) = listener.serve(stop).await {
      tracing::error!("run {id}: guest listener: {e:#}");
    }
  });
  Ok(())
}

/// Lets every user in the actor connect to the guest socket at `endpoint`:
/// the socket and its directory, which the server created private. The
/// directory is bind-mounted into the actor; on the host the run's
/// directory around it stays private.
fn open_to_every_user(endpoint: &Endpoint) -> Result<()> {
  use std::os::unix::fs::PermissionsExt;
  let Endpoint::Unix(socket) = endpoint;
  let mut modes = vec![(socket.as_path(), 0o666)];
  if let Some(dir) = socket.parent() {
    modes.push((dir, 0o755));
  }
  for (path, mode) in modes {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
      .with_context(|| format!("cannot open {} to the actor", path.display()))?;
  }
  Ok(())
}

/// Serves the actor's gateway on its gateway channel until `actor` is
/// cancelled.
fn serve_gateway(run: &Run, channels: &Channels, actor: &CancellationToken) -> Result<()> {
  let Endpoint::Unix(path) = &channels.gateway;
  let network = &run.profile.actor.network;
  let mut inject = BTreeMap::new();
  for (host, injection) in &network.inject {
    // The daemon holds the credential; the actor never sees it.
    match std::env::var(&injection.secret) {
      Ok(value) => {
        inject.insert(host.clone(), (injection.header.clone(), value));
      }
      Err(e) => tracing::warn!("run {}: no {} header for {host}: {}: {e}", run.id, injection.header, injection.secret),
    }
  }
  let policy = gateway::Policy {
    allow: network.allow.clone(),
    upstream: network.upstream.clone(),
    inject,
  };
  let gateway = Arc::new(gateway::Gateway::new(policy, run.trace.clone(), Network::Actor, Some(run.injector.clone()), actor.clone()));
  // A socket left by an earlier daemon would make bind fail.
  if let Err(e) = std::fs::remove_file(path)
    && e.kind() != std::io::ErrorKind::NotFound
  {
    tracing::warn!("cannot remove {}: {e}", path.display());
  }
  let listener = tokio::net::UnixListener::bind(path).with_context(|| format!("cannot listen on {}", path.display()))?;
  {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).with_context(|| format!("cannot restrict {}", path.display()))?;
  }
  run.tasks.spawn(gateway.run(tokio_stream::wrappers::UnixListenerStream::new(listener)));
  Ok(())
}

/// Removes the sockets of an actor of kind `kind` from the run's directory
/// `dir`: those its channels leave there (the vsock device's and the host
/// ports' the guest connects to, under Firecracker).
pub fn remove_sockets(daemon: &Daemon, actor: &needy_config::profile::Actor, dir: &Path) {
  let runtime = match runtime(daemon, actor) {
    Ok(runtime) => runtime,
    // An actor whose runtime this daemon lacks never started and left none.
    Err(e) => {
      tracing::debug!("no actor sockets to remove from {}: {e:#}", dir.display());
      return;
    }
  };
  for path in runtime.channels(dir).sockets {
    if let Err(e) = std::fs::remove_file(&path)
      && e.kind() != std::io::ErrorKind::NotFound
    {
      tracing::warn!("cannot remove {}: {e}", path.display());
    }
  }
}

/// Waits for the actor to exit, which a VM does when the guest reboots after
/// the driving program, then stops the actor's tasks and finishes the run.
/// An actor that exits before the driving program reported its exit fails
/// the run instead, with the actor's exit and the end of its log as the
/// reason. `name` is the actor's runtime.
async fn watch(daemon: Daemon, run: Arc<Run>, name: &'static str, mut vm: Box<dyn RunningActor>, actor: CancellationToken) {
  let waited = tokio::select! {
    waited = vm.wait() => waited,
    () = actor.cancelled() => {
      // The run stopped: its VM stops with it.
      if let Err(e) = vm.stop(VM_STOP_GRACE).await {
        tracing::warn!("run {}: cannot stop {name} {}: {e}", run.id, vm.pid());
      }
      release_actor(&daemon, &run).await;
      return;
    }
  };
  release_actor(&daemon, &run).await;
  let exit = match waited {
    Ok(status) => {
      use std::os::unix::process::ExitStatusExt;
      ActorVmExit {
        code: status.code(),
        signal: status.signal().map(needy_core_executor::signals::name),
        error: None,
      }
    }
    Err(e) => ActorVmExit {
      code: None,
      signal: None,
      error: Some(e.to_string()),
    },
  };
  actor.cancel();
  run.emit(&Event::ActorVmExit(exit.clone()));
  if run.actor_exit().is_some() {
    if let Err(refused) = run.state.advance(RunState::Finished) {
      tracing::debug!("run {}: {refused}", run.id);
    }
  } else {
    let reason = vm_failure(name, &exit, &runtime::log(&run.dir));
    tracing::warn!("run {} failed: {reason}", run.id);
    run.fail(reason);
  }
  daemon.finish_run(&run).await;
  // `needy report` right after the end must see it.
  run.trace.flush().await;
}

/// Records in `state.db` that run `run`'s actor ended.
async fn release_actor(daemon: &Daemon, run: &Run) {
  let id = store::actor_instance(&run.id);
  if let Err(e) = daemon.state.set_instance_state(&id, InstanceState::Released, needy_common::time::now_ms()).await {
    tracing::warn!("run {}: cannot write the end of its actor to state.db: {e:#}", run.id);
  }
}

/// How many of the last lines of the VM's log a failure quotes.
const FAILURE_LOG_LINES: usize = 5;

/// Why a run whose VM exited before its driving program did failed: the
/// VM's exit and the last lines of the log Firecracker and the guest's
/// console share, on one line. `name` is the actor's runtime.
fn vm_failure(name: &str, exit: &ActorVmExit, log: &Path) -> String {
  let status = match exit {
    ActorVmExit {
      error: Some(error), ..
    } => format!("waiting for {name} failed: {error}"),
    ActorVmExit {
      code: Some(code), ..
    } => format!("{name} exited with code {code}"),
    ActorVmExit {
      signal: Some(signal),
      ..
    } => format!("{name} was killed by {signal}"),
    ActorVmExit { .. } => format!("{name} exited"),
  };
  let mut reason = format!("the actor exited before its driving program did: {status}");
  match std::fs::read(log) {
    Ok(bytes) => {
      let text = String::from_utf8_lossy(&bytes);
      let lines: Vec<&str> = text.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
      let tail = &lines[lines.len().saturating_sub(FAILURE_LOG_LINES)..];
      if !tail.is_empty() {
        reason.push_str("; its log ends: ");
        reason.push_str(&tail.join(" | "));
      }
    }
    Err(e) => reason.push_str(&format!("; cannot read {}: {e}", log.display())),
  }
  reason
}

#[cfg(test)]
#[path = "actor_test.rs"]
mod tests;
