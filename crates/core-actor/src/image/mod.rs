//! What an actor boots or runs from, shared by the drivers that use it:
//!
//! - [`vm`]: needy's kernel and root file system, for `linux-firecracker`
//!   and `macos-vz`.
//! - [`container`]: the container actor image, for `linux-container` and
//!   `macos-apple-container`.
//! - [`guest`]: the guest needy every driver brings into its actor, and the
//!   guest disk the VM drivers attach.
//! - [`base`]: the base ABI between an image and the guest needy, which every
//!   driver checks an image records before it starts an actor from it.

pub mod base;
pub mod container;
pub mod guest;
pub mod vm;
