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
//! - [`manifest`]: what a VM image is and where it came from, in its
//!   directory.
//! - [`root`]: the root an image definition's steps are built on, the
//!   base's stack digest whichever driver built them.
//! - [`feature`]: OCI Dev Container feature artifact fetcher and content cache.
//! - [`builtin`]: embedded needy-specific built-in Dev Container features.
//! - [`store`]: the layer store: an image's layers, snapshots and commits,
//!   and the layer disks the VM drivers boot a snapshot from.

pub mod base;
pub mod builtin;
pub mod container;
pub mod feature;
pub mod guest;
pub mod manifest;
pub mod root;
pub mod store;
pub mod vm;
