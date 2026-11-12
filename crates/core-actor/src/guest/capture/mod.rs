//! Captures: how an exec inside the actor is caught and handed to the
//! proxy (`super::proxy`, the exec domain's adapter, the same for every
//! capture). A capture is mechanism: it never decides. Which one an actor
//! has is its driver's `Facets::capture` (`needy_core::types::Capture`).
//!
//! | Capture | `Capture` | Drivers | Turned on by |
//! | --- | --- | --- | --- |
//! | [`binfmt`]: the guest kernel's `binfmt_needy` | `Kernel` | `linux-firecracker`, `macos-vz` | writing the module's parameters |
//! | [`shim`]: links made when the image was built | `Shims` | `linux-container`, `macos-apple-container` | writing [`shim::ROUTE_FILE`] |
//!
//! Both route the same execs: those of the route's user whose program has
//! one of the route's names. Init turns routing on once the actor is set
//! up, so its own helpers stay in the actor.
//!
//! See `docs/ai/references/architecture/2026-11-12-drivers-and-extension-points-design.md`.

pub mod binfmt;
pub mod shim;

/// Which execs go to the daemon: those of user `uid` whose program has one
/// of `names`, or every one of theirs when the names are `*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
  pub uid: u32,
  pub names: Vec<String>,
}

impl Route {
  /// Whether an exec by `uid` matched by `names` is routed, as
  /// `binfmt_needy`'s `listed` decides in a VM.
  pub fn routes(&self, uid: u32, names: &[String]) -> bool {
    if uid != self.uid {
      return false;
    }
    if self.names.len() == 1 && self.names[0] == "*" {
      return true;
    }
    names.iter().any(|name| self.names.contains(name))
  }
}

/// Turns routing on for `route` with this actor's capture: shims in a
/// container, the kernel's `binfmt_needy` in a VM. A capture that cannot be
/// turned on is logged, and every command stays in the actor.
pub(super) fn enable(route: &Route, container: bool) {
  if container {
    if let Err(e) = shim::enable(route) {
      tracing::warn!("cannot turn routing on; every command stays in the actor: {e}");
    }
  } else {
    binfmt::enable(route);
  }
}
