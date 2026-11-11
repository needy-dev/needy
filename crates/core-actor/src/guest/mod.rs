//! Inside an actor: the init process and the exec-routing proxy.
//!
//! In a microVM the kernel starts `/.needy/init` as PID 1, and its
//! `binfmt_needy` hands routed execs to `/.needy/proxy`; both talk to the
//! daemon over vsock (host CID 2). The kernel never routes execs made by
//! programs in `/.needy`, so init's helpers and the proxy's local exec stay
//! in the actor.
//!
//! In a container, Docker starts `/.needy/init` as PID 1, the daemon's
//! channels are Unix sockets bind-mounted at [`CHANNELS`], and routing is by
//! build-time shims (see [`shim`]): every program on the image's `PATH` is a
//! link to this binary, which runs the real program from `/.needy/real`
//! unless the run routes it.
//!
//! The guest does not use `needy-trace`: it sends `needy_core::event`
//! payloads to the daemon through `GuestService`.

mod ancestry;
mod init;
mod proxy;
pub mod shim;

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use needy_api_client::Client;

pub use crate::host::container::CHANNELS;
use crate::host::container::{GATEWAY_SOCKET, GUEST_SOCKET, PROCESS_SOCKET};

/// The daemon's guest listener, as a VM's actor reaches it.
// Source: https://github.com/torvalds/linux/blob/v6.18/include/uapi/linux/vm_sockets.h#L126-L131
// (`VMADDR_CID_HOST` is 2). Port 7000 is needy's own choice,
// `GUEST_PORT` in `crates/core-actor/src/host/firecracker.rs`.
const DAEMON: &str = "vsock://2:7000";

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
    DAEMON.to_string()
  };
  // NOTICE: 5 s is well past a VM's boot time (about 200 ms); the daemon
  // binds a container's socket before it creates the container.
  needy_api_client::ready::wait(&endpoint, Duration::from_secs(5)).await.with_context(|| format!("cannot reach the daemon at {endpoint}"))
}
