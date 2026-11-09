//! Wires a session to its actor: the guest listener the actor's init and
//! proxy call, the gateway its network goes through, and the Firecracker VM.
//!
//! The actor's tasks belong to its session and run under a child of the
//! session's token, which the VM's exit also cancels: the guest listener and
//! the gateway stop when the actor exits or its session stops.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use needy_api_server::control::Caller;
use needy_api_server::{Health, Listen, Server};
use needy_core::event::{ActorStart, ActorVmExit, Event};
use needy_core::types::{Network, SessionState};
use needy_core_actor::host::{gateway, vm};
use tokio_util::sync::CancellationToken;

use crate::Daemon;
use crate::session::Session;

/// Ports the actor reaches on the host over vsock.
pub const GUEST_PORT: u32 = 7000;
pub const GATEWAY_PORT: u32 = 7001;

// NOTICE: the guest's state is not kept, so Firecracker gets a short time to
// exit on `SIGTERM` before it is killed; a session's stop waits this long at
// most for its VM.
const VM_STOP_GRACE: Duration = Duration::from_secs(3);

/// Starts the actor of `session`: its guest listener, its gateway, then its
/// VM.
pub async fn start(daemon: &Daemon, session: &Arc<Session>) -> Result<()> {
  let image = vm::Image::new(&daemon.config.actor_image);
  image.check()?;
  let launch = vm::Launch::new(&image, &session.dir, session.recipe.as_ref());
  let actor = session.cancel.child_token();
  if let Err(e) = serve_guest(daemon, session, &actor).await {
    actor.cancel();
    return Err(e);
  }
  if let Err(e) = serve_gateway(session, &actor) {
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
  session.emit(&Event::ActorStart(ActorStart {
    pid: vm.pid(),
    vcpus: launch.vcpus,
    mem_mib: launch.mem_mib,
  }));
  session.tasks.spawn(watch(session.clone(), vm, actor));
  Ok(())
}

/// Serves the session's API to the actor on its vsock port, as
/// `Caller::Actor` bound to the session, until `actor` is cancelled.
async fn serve_guest(daemon: &Daemon, session: &Session, actor: &CancellationToken) -> Result<()> {
  let api = Arc::new(daemon.clone());
  let health = Health {
    instance_id: uuid::Uuid::new_v4().to_string(),
    role: "control",
  };
  let caller = Caller::Actor {
    session: session.id.clone(),
  };
  let listener = Server::new(health, caller)
    .sessions(api.clone())
    .processes(api.clone())
    .guest(api)
    .bind(&[Listen::Unix(vm::host_port(&session.dir, GUEST_PORT))])
    .await?;
  let id = session.id.clone();
  let stop = actor.clone();
  session.tasks.spawn(async move {
    if let Err(e) = listener.serve(stop).await {
      tracing::error!("session {id}: guest listener: {e:#}");
    }
  });
  Ok(())
}

/// Serves the actor's gateway on its vsock port until `actor` is cancelled.
fn serve_gateway(session: &Session, actor: &CancellationToken) -> Result<()> {
  let path = vm::host_port(&session.dir, GATEWAY_PORT);
  let network = &session.profile.actor.network;
  let mut inject = BTreeMap::new();
  for (host, injection) in &network.inject {
    // The daemon holds the credential; the actor never sees it.
    match std::env::var(&injection.secret) {
      Ok(value) => {
        inject.insert(host.clone(), (injection.header.clone(), value));
      }
      Err(e) => tracing::warn!("session {}: no {} header for {host}: {}: {e}", session.id, injection.header, injection.secret),
    }
  }
  let policy = gateway::Policy {
    allow: network.allow.iter().map(|glob| glob.as_str().to_string()).collect(),
    upstream: network.upstream.clone(),
    inject,
  };
  let gateway =
    Arc::new(gateway::Gateway::new(policy, session.trace.clone(), Network::Actor, Some(session.injector.clone()), actor.clone()));
  // A socket left by an earlier daemon would make bind fail.
  if let Err(e) = std::fs::remove_file(&path)
    && e.kind() != std::io::ErrorKind::NotFound
  {
    tracing::warn!("cannot remove {}: {e}", path.display());
  }
  let listener = tokio::net::UnixListener::bind(&path).with_context(|| format!("cannot listen on {}", path.display()))?;
  session.tasks.spawn(gateway.run(tokio_stream::wrappers::UnixListenerStream::new(listener)));
  Ok(())
}

/// Removes the actor's sockets from the session's directory `dir`: the vsock
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
/// driving program, then finishes the session and stops the actor's tasks.
async fn watch(session: Arc<Session>, mut vm: vm::Vm, actor: CancellationToken) {
  let waited = tokio::select! {
    waited = vm.wait() => waited,
    () = actor.cancelled() => {
      // The session stopped: its VM stops with it.
      if let Err(e) = vm.stop(VM_STOP_GRACE).await {
        tracing::warn!("session {}: cannot stop firecracker {}: {e}", session.id, vm.pid());
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
  session.emit(&Event::ActorVmExit(exit));
  if let Err(refused) = session.state.advance(SessionState::Finished) {
    tracing::debug!("session {}: {refused}", session.id);
  }
  // TODO(8h): the session keeps its executors and the actor's sockets until
  // `session stop`; slice 8h releases them here and keeps the session listed
  // as finished.
}

#[cfg(test)]
#[path = "actor_test.rs"]
mod tests;
