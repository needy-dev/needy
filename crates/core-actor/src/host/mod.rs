//! The actor's host side: what runs next to the actor on the daemon's
//! machine.

#[cfg(unix)]
pub mod container;
pub mod detect;
pub mod firecracker;
pub mod gateway;
#[cfg(unix)]
pub mod identity;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod runtime;
