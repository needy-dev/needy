//! The contract between an actor's guest and the host side: the vsock ports
//! a VM's guest dials or serves, the channel sockets a container actor
//! finds at fixed paths, and where needy's own programs are in an actor
//! image. The guest (`guest/`), the CLI's dispatch by `argv[0]` and every
//! driver read them here, so no driver borrows them from another.

use std::path::{Path, PathBuf};

/// Ports the actor reaches on the host over vsock.
// NOTICE: needy's own choices. The guest dials `GUEST_PORT` on the host's
// CID (`crates/core-actor/src/guest/mod.rs`); the daemon tells it the other
// two in its configuration.
pub const GUEST_PORT: u32 = 7000;
pub const GATEWAY_PORT: u32 = 7001;
/// The port the guest serves `ProcessService` on for a driving program on
/// a terminal; the host connects to it.
pub const PROCESS_PORT: u32 = 7002;

/// Where the actor's channels are in the container.
pub const CHANNELS: &str = "/.needy/channels";
/// The daemon's guest API, in the channels directory.
pub const GUEST_SOCKET: &str = "guest.sock";
/// The actor gateway, in the channels directory.
pub const GATEWAY_SOCKET: &str = "gateway.sock";
/// The driving program's process service, which init serves.
pub const PROCESS_SOCKET: &str = "process.sock";

/// needy's own programs in an actor: the kernel never routes an exec they
/// make (`binfmt_needy`'s `caller_is_needy`, `images/kernel/binfmt_needy.c`),
/// and a shim never stands for one.
pub const NEEDY_DIR: &str = "/.needy";
/// The actor's init, which the kernel or the container runtime starts as
/// PID 1.
pub const INIT: &str = "/.needy/init";
/// The proxy a caught exec is handed to. `binfmt_needy`'s `proxy`
/// parameter has the same default (`images/kernel/binfmt_needy.c`).
pub const PROXY: &str = "/.needy/proxy";
/// Where a container actor image's shim stage (`images/container/shim.sh`)
/// keeps the real programs, by their original paths.
pub const REAL: &str = "/.needy/real";

/// The channels directory in the actor's directory `dir`.
pub fn channels_dir(dir: &Path) -> PathBuf {
  dir.join("channels")
}
