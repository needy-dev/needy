//! Route stubs. The kernel can only route a file that exists, so routed
//! commands the actor does not have get empty placeholder files on the
//! driving program's `PATH`.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// The stubs' directory, last on the driving program's `PATH`.
pub(super) const STUB_DIR: &str = "/run/needy/bin";
/// The actor's own programs, searched before the stubs.
pub(super) const SYSTEM_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// Creates a stub for each plain command name the actor lacks.
pub(super) fn create(names: &[String]) {
  let dir = Path::new(STUB_DIR);
  if let Err(e) = std::fs::create_dir_all(dir) {
    tracing::warn!("cannot create {STUB_DIR}; routed commands the actor lacks are not found: {e}");
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
    let created = std::fs::write(&stub, b"").and_then(|()| std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)));
    if let Err(e) = created {
      tracing::warn!("cannot create the route stub {}: {e}", stub.display());
    }
  }
}
