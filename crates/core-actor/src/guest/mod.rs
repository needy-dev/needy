//! Inside an actor: the init process, the captures that catch an exec, and
//! the exec-routing proxy they hand it to.
//!
//! This code runs as the guest needy, `/.needy/needy`, which the daemon
//! injects into a base image when the actor starts. The image's own
//! programs in `/.needy` are two stubs that hand over to it (the base ABI,
//! `crate::image::base`): the boot stub `/.needy/init` starts
//! `/.needy/needy init` ([`init_main`]), and the shim stub `/.needy/proxy`
//! starts it with its own path and the exec's ([`Stub`], [`stub_main`]).
//!
//! In a microVM the kernel starts the boot stub as PID 1, which mounts the
//! guest disk first, and its `binfmt_needy` ([`capture::binfmt`]) hands
//! routed execs to the shim stub at `/.needy/proxy`; init and the proxy talk
//! to the daemon over vsock (the host's CID). The kernel never routes execs
//! made by programs in `/.needy`, so init's helpers and the proxy's local
//! exec stay in the actor.
//!
//! In a container, the engine starts the boot stub as PID 1, the daemon's
//! channels are Unix sockets bind-mounted at [`CHANNELS`], and routing is by
//! build-time shims (see [`capture::shim`]): every program on the image's
//! `PATH` is a link to the shim stub, which hands over to this binary, which
//! runs the real program from `/.needy/real` unless the run routes it.
//!
//! The paths and ports are the guest's side of [`crate::contract`].
//!
//! The guest does not use `needy-trace`: it sends `needy_core::event`
//! payloads to the daemon through `GuestService`.

mod ancestry;
pub mod capture;
mod init;
mod proxy;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use needy_api_client::Client;

pub use crate::contract::CHANNELS;
use crate::contract::{GATEWAY_SOCKET, GUEST_PORT, GUEST_SOCKET, PROCESS_SOCKET, PROXY, STUB_EXE, STUB_EXECFN};

/// The daemon's guest listener, as a VM's actor reaches it: the host's CID
/// and [`GUEST_PORT`].
// Source: https://github.com/torvalds/linux/blob/v6.18/include/uapi/linux/vm_sockets.h#L126-L131
// (`VMADDR_CID_HOST` is 2), which `libc::VMADDR_CID_HOST` carries.
fn guest_api() -> String {
  format!("vsock://{}:{GUEST_PORT}", libc::VMADDR_CID_HOST)
}

/// The actor's init: sets the machine up, runs the driving program, and
/// reboots, which ends the VM, or exits, which ends the container. The boot
/// stub starts it as `/.needy/needy init`.
pub fn init_main() -> ! {
  init::main()
}

/// What the shim stub (`images/stub/shim.c`) hands the guest needy, in
/// [`STUB_EXE`] and [`STUB_EXECFN`]: the path it was started through, and
/// the path the original `execve` named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stub {
  /// `/.needy/proxy`, or a shim's path on `PATH`.
  pub exe: PathBuf,
  /// What `AT_EXECFN` was for the stub.
  pub execfn: String,
}

impl Stub {
  /// The stub's hand-off, taken out of this process's environment so no
  /// program the guest needy runs, and no routed command's environment,
  /// sees it. `None` when no stub started this process.
  ///
  /// # Safety
  ///
  /// It changes the environment, so no other thread may exist: call it
  /// first thing in `main`.
  pub unsafe fn take() -> Option<Self> {
    let exe = std::env::var_os(STUB_EXE)?;
    let execfn = std::env::var_os(STUB_EXECFN).map(|v| v.to_string_lossy().into_owned()).unwrap_or_default();
    // SAFETY: the caller guarantees this process has one thread.
    unsafe {
      std::env::remove_var(STUB_EXE);
      std::env::remove_var(STUB_EXECFN);
    }
    Some(Self {
      exe: PathBuf::from(exe),
      execfn,
    })
  }
}

/// What a stub started: the proxy (`/.needy/proxy`, through which the
/// kernel's `binfmt_needy` and the route stubs hand over an exec) or a shim
/// (a program on a container image's `PATH`). argv is the original
/// command's.
pub fn stub_main(stub: Stub) -> ! {
  if stub.exe == Path::new(PROXY) {
    proxy::main(stub.execfn)
  }
  if capture::shim::real(&stub.exe).is_some() {
    capture::shim::main(stub)
  }
  eprintln!("needy: started by a stub at {}, which is neither {PROXY} nor a shim", stub.exe.display());
  std::process::exit(127)
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
