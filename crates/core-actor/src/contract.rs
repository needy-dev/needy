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

/// What the actor gateway writes on a connection before it closes it for a
/// `reset` fault, as the first bytes on that connection: the guest's
/// gateway forwarder then resets the driving program's connection itself.
// NOTICE: needy's own choice. It starts with a NUL byte, so it is never the
// start of the HTTP response a gateway sends first on any other connection
// ("HTTP/1.1 ..."). A close alone does not reach a `macos-vz` guest: vfkit's
// vsock relay passes no half-close to it (`VirtioSocketConnection` has no
// `CloseWrite`):
// https://github.com/crc-org/vfkit/blob/v0.6.4/pkg/vf/vsock.go#L77-L120
// https://github.com/Code-Hex/vz/blob/v3.7.1/socket.go#L285-L299
pub const GATEWAY_RESET: &[u8] = b"\0needy-reset\0";

/// needy's own programs in an actor: the kernel never routes an exec they
/// make (`binfmt_needy`'s `caller_is_needy`, `images/kernel/binfmt_needy.c`),
/// and a shim never stands for one.
///
/// These paths, the stubs' hand-off and the guest disk are the base ABI
/// (`crate::image::base`), which a base image and the guest needy share:
/// see `docs/ai/references/architecture/2026-11-12-actor-images-design.md`.
pub const NEEDY_DIR: &str = "/.needy";
/// The boot stub (`images/stub/boot.c`), which the kernel or the container
/// runtime starts as PID 1. It starts the guest needy as init.
pub const INIT: &str = "/.needy/init";
/// The shim stub (`images/stub/shim.c`), which a caught exec is handed to.
/// `binfmt_needy`'s `proxy` parameter has the same default
/// (`images/kernel/binfmt_needy.c`); in a container image every program on
/// `PATH` is a hard link to it.
pub const PROXY: &str = "/.needy/proxy";
/// The guest needy, which the daemon injects when an actor starts: an empty
/// placeholder in the base image.
pub const GUEST: &str = "/.needy/needy";
/// The argument the boot stub starts the guest needy with: `/.needy/needy
/// init`.
pub const INIT_ARG: &str = "init";
/// What the shim stub passes on to the guest needy: the path it was started
/// through (`/proc/self/exe`), and the path the original `execve` named
/// (`AT_EXECFN`).
pub const STUB_EXE: &str = "NEEDY_STUB_EXE";
pub const STUB_EXECFN: &str = "NEEDY_STUB_EXECFN";
/// The base ABI version of the image, as text, in its root file system.
pub const BASE_ABI: &str = "/.needy/base-abi";
/// Where a container actor image's shim stage (`images/container/shim.sh`)
/// keeps the real programs, by their original paths.
pub const REAL: &str = "/.needy/real";

/// The channels directory in the actor's directory `dir`.
pub fn channels_dir(dir: &Path) -> PathBuf {
  dir.join("channels")
}
