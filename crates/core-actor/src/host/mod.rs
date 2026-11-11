//! The actor's host side: what runs next to the actor on the daemon's
//! machine.

#[cfg(unix)]
pub mod apple_container;
#[cfg(unix)]
pub mod container;
pub mod detect;
pub mod firecracker;
pub mod gateway;
#[cfg(unix)]
pub mod identity;
pub mod runtime;
pub mod vz;
