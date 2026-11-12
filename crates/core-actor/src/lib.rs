//! The actor: the guest that runs inside its microVM or container, and the
//! host side that runs next to it. Everything outside [`guest`] is the host
//! side.
//!
//! - [`driver`]: the actor drivers, what starts, waits for and stops an
//!   actor (`linux-firecracker`, `linux-container`, `macos-vz`,
//!   `macos-apple-container`), with the table of them.
//! - [`sidecar`]: the services the daemon runs for one run next to the
//!   actor (the gateway; the guest API lives in the daemon).
//! - [`contract`]: what the guest and the host side agree on: vsock ports,
//!   channel paths and socket names.
//! - [`image`]: what the drivers boot or run: needy's VM image, the
//!   container actor image.
//! - [`identity`]: a recipe's identity files presented from the host.
//! - [`guest`] (Linux only): the in-guest init and the exec-routing proxy.
//! - `sshfs` (Linux only): workspace mounts over a needy exec stream, made
//!   by a VM's init or, for a container, on the daemon's machine.

pub mod contract;
pub mod driver;
#[cfg(target_os = "linux")]
pub mod guest;
#[cfg(unix)]
pub mod identity;
pub mod image;
pub mod sidecar;
#[cfg(target_os = "linux")]
mod sshfs;
