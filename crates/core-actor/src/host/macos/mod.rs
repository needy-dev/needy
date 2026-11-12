//! The actor runtimes of macOS on Apple silicon, named `macos-*` in
//! profiles: `macos-vz` (`vz`, needy's guest in a Virtualization.framework VM
//! run by vfkit) and `macos-apple-container` (`apple_container`, the actor in
//! a container of Apple's `container`).

#[cfg(unix)]
pub mod apple_container;
pub mod vz;
