//! Route stubs. The kernel can only route a file that exists, so routed
//! commands the actor does not have get empty placeholder files on the
//! driving program's `PATH`. A container has no such kernel, so its stubs are
//! links to the proxy, which asks the daemon for every one.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

// A container's stubs link to the proxy.
use crate::contract::PROXY;

pub(super) use crate::guest::capture::shim::SYSTEM_PATH;
use crate::guest::capture::shim::TRAMPOLINE_DIR;

/// The stubs' directory, last on the driving program's `PATH`.
pub(in crate::guest) const STUB_DIR: &str = "/run/needy/bin";

/// The driving program's `PATH`: the trampolines when init made them (in a
/// container), the actor's own programs, then the stubs.
pub(super) fn search_path() -> String {
  if Path::new(TRAMPOLINE_DIR).is_dir() {
    format!("{TRAMPOLINE_DIR}:{SYSTEM_PATH}:{STUB_DIR}")
  } else {
    format!("{SYSTEM_PATH}:{STUB_DIR}")
  }
}

/// Creates a stub for each plain command name the actor lacks: an empty
/// file in a VM, a link to the proxy in a container.
pub(super) fn create(names: &[String], container: bool) {
  create_in(Path::new(STUB_DIR), names, container);
}

/// [`create`] in `dir`. A stub that exists stays as it is, and a new one
/// appears whole: it is written under a temporary name and renamed into
/// place, since executing a file that is open for writing fails with
/// `ETXTBSY` and the route list can come again while the program runs.
fn create_in(dir: &Path, names: &[String], container: bool) {
  if let Err(e) = std::fs::create_dir_all(dir) {
    tracing::warn!("cannot create {}; routed commands the actor lacks are not found: {e}", dir.display());
    return;
  }
  for name in names {
    if name.contains('/') || name == "*" || name.is_empty() {
      continue;
    }
    if SYSTEM_PATH.split(':').any(|d| Path::new(d).join(name).exists()) {
      continue;
    }
    let stub = dir.join(name);
    if stub.symlink_metadata().is_ok() {
      continue;
    }
    let temporary = dir.join(format!(".{name}.new"));
    // One a start that died midway left; usually there is none.
    let _ = std::fs::remove_file(&temporary);
    let created = if container {
      std::os::unix::fs::symlink(PROXY, &temporary)
    } else {
      std::fs::write(&temporary, b"").and_then(|()| std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o755)))
    };
    if let Err(e) = created.and_then(|()| std::fs::rename(&temporary, &stub)) {
      tracing::warn!("cannot create the route stub {}: {e}", stub.display());
      // Nothing to clean up when the temporary file was never made.
      let _ = std::fs::remove_file(&temporary);
    }
  }
}

#[cfg(test)]
#[path = "stubs_test.rs"]
mod tests;
