//! The actor: the guest that runs inside its microVM or container, and the
//! host side that runs next to it.
//!
//! - [`guest`] (Linux only): the in-guest init and the exec-routing proxy.
//! - [`host`]: what this machine offers, the actor runtimes (Firecracker, a
//!   container through Docker), and the actor gateway.
//! - `sshfs` (Linux only): workspace mounts over a needy exec stream, made
//!   by a VM's init or, for a container, on the daemon's machine.

#[cfg(target_os = "linux")]
pub mod guest;
pub mod host;
/// The experimental `macos-native` actor's init and shims (see
/// [`host::macos`]).
#[cfg(target_os = "macos")]
pub mod native;
#[cfg(target_os = "linux")]
mod sshfs;
