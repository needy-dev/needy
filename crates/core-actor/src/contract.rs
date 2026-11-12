//! The contract between an actor's guest and the host side: the vsock ports
//! a VM's guest dials or serves, and the channel sockets a container actor
//! finds at fixed paths. The guest (`guest/`) and every driver read them
//! here, so no driver borrows them from another.

use std::path::{Path, PathBuf};

/// Ports the actor reaches on the host over vsock.
// NOTICE: needy's own choices; the guest's init dials `GUEST_PORT` as
// `vsock://2:7000` in `crates/core-actor/src/guest/mod.rs`.
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

/// The channels directory in the actor's directory `dir`.
pub fn channels_dir(dir: &Path) -> PathBuf {
  dir.join("channels")
}
