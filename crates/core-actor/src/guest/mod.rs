//! Inside an actor: the init process, the captures that catch an exec, and
//! the exec-routing proxy they hand it to.
//!
//! In a microVM the kernel starts `/.needy/init` as PID 1, and its
//! `binfmt_needy` ([`capture::binfmt`]) hands routed execs to
//! `/.needy/proxy`; both talk to the daemon over vsock (the host's CID). The
//! kernel never routes execs made by programs in `/.needy`, so init's
//! helpers and the proxy's local exec stay in the actor.
//!
//! In a container, the engine starts `/.needy/init` as PID 1, the daemon's
//! channels are Unix sockets bind-mounted at [`CHANNELS`], and routing is by
//! build-time shims (see [`capture::shim`]): every program on the image's
//! `PATH` is a link to this binary, which runs the real program from
//! `/.needy/real` unless the run routes it.
//!
//! The paths and ports are the guest's side of [`crate::contract`].
//!
//! The guest does not use `needy-trace`: it sends `needy_core::event`
//! payloads to the daemon through `GuestService`.

mod ancestry;
pub mod capture;
mod init;
mod proxy;

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use needy_api_client::Client;

pub use crate::contract::CHANNELS;
use crate::contract::{GATEWAY_SOCKET, GUEST_PORT, GUEST_SOCKET, PROCESS_SOCKET};

/// The daemon's guest listener, as a VM's actor reaches it: the host's CID
/// and [`GUEST_PORT`].
// Source: https://github.com/torvalds/linux/blob/v6.18/include/uapi/linux/vm_sockets.h#L126-L131
// (`VMADDR_CID_HOST` is 2), which `libc::VMADDR_CID_HOST` carries.
fn guest_api() -> String {
  format!("vsock://{}:{GUEST_PORT}", libc::VMADDR_CID_HOST)
}

/// The actor's init: sets the machine up, runs the driving program, and
/// reboots, which ends the VM, or exits, which ends the container.
pub fn init_main() -> ! {
  init::main()
}

/// The proxy for a routed exec; argv is the original command's.
pub fn proxy_main() -> ! {
  proxy::main()
}

/// Whether this actor is a container: the daemon's channels are
/// bind-mounted where a VM has none.
fn in_container() -> bool {
  Path::new(CHANNELS).join(GUEST_SOCKET).exists()
}

/// Where a container actor's init reaches the gateway, and serves the
/// driving program's process service.
fn container_socket(name: &str) -> std::path::PathBuf {
  Path::new(CHANNELS).join(name)
}

fn gateway_socket() -> std::path::PathBuf {
  container_socket(GATEWAY_SOCKET)
}

fn process_socket() -> std::path::PathBuf {
  container_socket(PROCESS_SOCKET)
}

fn runtime() -> tokio::runtime::Runtime {
  // NOTICE: without a runtime neither init nor the proxy can do anything;
  // building one fails only when the system is out of threads or memory.
  tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime")
}

/// Connects to the daemon, which may still be binding its listener while
/// the actor starts.
async fn daemon() -> Result<Client> {
  let endpoint = if in_container() {
    format!("unix:{}", container_socket(GUEST_SOCKET).display())
  } else {
    guest_api()
  };
  // NOTICE: 5 s is well past a VM's boot time (about 200 ms); the daemon
  // binds a container's socket before it creates the container.
  needy_api_client::ready::wait(&endpoint, Duration::from_secs(5)).await.with_context(|| format!("cannot reach the daemon at {endpoint}"))
}
