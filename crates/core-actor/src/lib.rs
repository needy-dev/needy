//! The actor: the guest that runs inside its microVM, and the host side that
//! runs next to it.
//!
//! - [`guest`] (Linux only): the in-guest init and the exec-routing proxy.
//! - [`host`]: what this machine offers, the Firecracker VM, and the actor
//!   gateway.

#[cfg(target_os = "linux")]
pub mod guest;
pub mod host;
