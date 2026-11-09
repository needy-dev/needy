//! Inside an actor microVM: the init process and the exec-routing proxy.
//!
//! The kernel starts `/.needy/init` as PID 1, and its `binfmt_needy` hands
//! routed execs to `/.needy/proxy`; both are this binary. Both talk to the
//! daemon over vsock (host CID 2). The kernel never routes execs made by
//! programs in `/.needy`, so init's helpers and the proxy's local exec stay
//! in the actor.
//!
//! The guest does not use `needy-trace`: it sends `needy_core::event`
//! payloads to the daemon through `GuestService`.

mod init;
mod proxy;

use std::time::Duration;

use anyhow::{Context, Result};
use backon::{ConstantBuilder, Retryable};
use needy_api_client::Client;

/// The daemon's guest listener, as the actor reaches it.
const DAEMON: &str = "vsock://2:7000";

/// The actor's init: sets the machine up, runs the driving program, and
/// reboots, which ends the VM.
pub fn init_main() -> ! {
  init::main()
}

/// The proxy for a routed exec; argv is the original command's.
pub fn proxy_main() -> ! {
  proxy::main()
}

fn runtime() -> tokio::runtime::Runtime {
  // NOTICE: without a runtime neither init nor the proxy can do anything;
  // building one fails only when the system is out of threads or memory.
  tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime")
}

/// Connects to the daemon, which may still be binding the vsock listener
/// while the VM boots.
async fn daemon() -> Result<Client> {
  // NOTICE: 50 retries 100 ms apart give the daemon 5 s, well past a VM's
  // boot time (about 200 ms).
  let retry = ConstantBuilder::default().with_delay(Duration::from_millis(100)).with_max_times(50);
  (|| Client::connect(DAEMON)).retry(retry).await.context("cannot reach the daemon over vsock")
}
