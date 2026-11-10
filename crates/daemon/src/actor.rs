//! Wires a run to its actor: the guest listener the actor's init and
//! proxy call, the gateway its network goes through, and the Firecracker VM.
//!
//! The actor's tasks belong to its run and run under a child of the
//! run's token, which the VM's exit also cancels: the guest listener and
//! the gateway stop when the actor exits or its run stops.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use needy_api_server::control::Caller;
use needy_api_server::{Health, Listen, Server};
use needy_core::event::{ActorStart, ActorVmExit, Event};
use needy_core::types::{Network, RunState};
use needy_core_actor::host::{gateway, vm};
use tokio_util::sync::CancellationToken;

use crate::Daemon;
use crate::run::Run;

/// Ports the actor reaches on the host over vsock.
pub const GUEST_PORT: u32 = 7000;
pub const GATEWAY_PORT: u32 = 7001;

// NOTICE: the guest's state is not kept, so Firecracker gets a short time to
// exit on `SIGTERM` before it is killed; a run's stop waits this long at
// most for its VM.
const VM_STOP_GRACE: Duration = Duration::from_secs(3);

/// Fails unless this host can run an actor's VM. A create checks this
/// before it acquires anything, so a host without KVM fails at once.
pub fn check_host(daemon: &Daemon) -> Result<()> {
  vm::check_kvm(&daemon.config.kvm)?;
  Ok(())
}

/// Starts the actor of `run`: its guest listener, its gateway, then its
/// VM.
pub async fn start(daemon: &Daemon, run: &Arc<Run>) -> Result<()> {
  let image = vm::Image::new(&daemon.config.actor_image);
  image.check()?;
  let launch = vm::Launch::new(&image, &run.dir, run.recipe.as_ref());
  let actor = run.cancel.child_token();
  if let Err(e) = serve_guest(daemon, run, &actor).await {
    actor.cancel();
    return Err(e);
  }
  if let Err(e) = serve_gateway(run, &actor) {
    actor.cancel();
    return Err(e);
  }
  let vm = match vm::Vm::start(&launch) {
    Ok(vm) => vm,
    Err(e) => {
      actor.cancel();
      return Err(e.into());
    }
  };
  run.emit(&Event::ActorStart(ActorStart {
    pid: vm.pid(),
    vcpus: launch.vcpus,
    mem_mib: launch.mem_mib,
  }));
  run.tasks.spawn(watch(daemon.clone(), run.clone(), vm, actor));
  Ok(())
}

/// Serves the run's API to the actor on its vsock port, as
/// `Caller::Actor` bound to the run, until `actor` is cancelled.
async fn serve_guest(daemon: &Daemon, run: &Run, actor: &CancellationToken) -> Result<()> {
  let api = Arc::new(daemon.clone());
  let health = Health {
    instance_id: uuid::Uuid::new_v4().to_string(),
    role: "control",
  };
  let caller = Caller::Actor {
    run: run.id.clone(),
  };
  let listener = Server::new(health, caller)
    .runs(api.clone())
    .processes(api.clone())
    .guest(api)
    .bind(&[Listen::Unix(vm::host_port(&run.dir, GUEST_PORT))])
    .await?;
  let id = run.id.clone();
  let stop = actor.clone();
  run.tasks.spawn(async move {
    if let Err(e) = listener.serve(stop).await {
      tracing::error!("run {id}: guest listener: {e:#}");
    }
  });
  Ok(())
}

/// Serves the actor's gateway on its vsock port until `actor` is cancelled.
fn serve_gateway(run: &Run, actor: &CancellationToken) -> Result<()> {
  let path = vm::host_port(&run.dir, GATEWAY_PORT);
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
    allow: network.allow.iter().map(|glob| glob.as_str().to_string()).collect(),
    upstream: network.upstream.clone(),
    inject,
  };
  let gateway = Arc::new(gateway::Gateway::new(policy, run.trace.clone(), Network::Actor, Some(run.injector.clone()), actor.clone()));
  // A socket left by an earlier daemon would make bind fail.
  if let Err(e) = std::fs::remove_file(&path)
    && e.kind() != std::io::ErrorKind::NotFound
  {
    tracing::warn!("cannot remove {}: {e}", path.display());
  }
  let listener = tokio::net::UnixListener::bind(&path).with_context(|| format!("cannot listen on {}", path.display()))?;
  {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
      .with_context(|| format!("cannot restrict {}", path.display()))?;
  }
  run.tasks.spawn(gateway.run(tokio_stream::wrappers::UnixListenerStream::new(listener)));
  Ok(())
}

/// Removes the actor's sockets from the run's directory `dir`: the vsock
/// device's and the host ports' the guest connects to.
pub fn remove_sockets(dir: &Path) {
  for path in [
    vm::vsock(dir),
    vm::host_port(dir, GUEST_PORT),
    vm::host_port(dir, GATEWAY_PORT),
  ] {
    if let Err(e) = std::fs::remove_file(&path)
      && e.kind() != std::io::ErrorKind::NotFound
    {
      tracing::warn!("cannot remove {}: {e}", path.display());
    }
  }
}

/// Waits for the VM to exit, which it does when the guest reboots after the
/// driving program, then stops the actor's tasks and finishes the run.
/// A VM that exits before the driving program reported its exit fails the
/// run instead, with the VM's exit and the end of its log as the reason.
async fn watch(daemon: Daemon, run: Arc<Run>, mut vm: vm::Vm, actor: CancellationToken) {
  let waited = tokio::select! {
    waited = vm.wait() => waited,
    () = actor.cancelled() => {
      // The run stopped: its VM stops with it.
      if let Err(e) = vm.stop(VM_STOP_GRACE).await {
        tracing::warn!("run {}: cannot stop firecracker {}: {e}", run.id, vm.pid());
      }
      return;
    }
  };
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
    let reason = vm_failure(&exit, &vm::console(&run.dir));
    tracing::warn!("run {} failed: {reason}", run.id);
    run.fail(reason);
  }
  daemon.finish_run(&run).await;
  // `needy report` right after the end must see it.
  run.trace.flush().await;
}

/// How many of the last lines of the VM's log a failure quotes.
const FAILURE_LOG_LINES: usize = 5;

/// Why a run whose VM exited before its driving program did failed: the
/// VM's exit and the last lines of the log Firecracker and the guest's
/// console share, on one line.
fn vm_failure(exit: &ActorVmExit, log: &Path) -> String {
  let status = match exit {
    ActorVmExit {
      error: Some(error), ..
    } => format!("waiting for firecracker failed: {error}"),
    ActorVmExit {
      code: Some(code), ..
    } => format!("firecracker exited with code {code}"),
    ActorVmExit {
      signal: Some(signal),
      ..
    } => format!("firecracker was killed by {signal}"),
    ActorVmExit { .. } => "firecracker exited".to_string(),
  };
  let mut reason = format!("the actor's VM exited before its driving program did: {status}");
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
