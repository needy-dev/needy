//! What an actor boots or runs from, shared by the drivers that use it:
//!
//! - [`vm`]: needy's kernel and root file system, for `linux-firecracker`
//!   and `macos-vz`.
//! - [`container`]: the container actor image, for `linux-container` and
//!   `macos-apple-container`.

pub mod container;
pub mod vm;
