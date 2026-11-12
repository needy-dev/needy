//! The profile's user in the actor's account files, so that `whoami`, `id`
//! and `getent passwd` name the user that `HOME`, `USER` and `LOGNAME` do.
//!
//! The image has its own user (`agent`, uid 1000) and a read-only root, so
//! init writes the rewritten files to a tmpfs under `/run` and bind-mounts
//! each over its `/etc` file. A file that already names the user exactly,
//! as a `linux-container` actor's do (its driver binds them from the host,
//! since init there may not mount), is left alone.

use std::path::Path;

use needy_common::account::{Account, FILES};
use nix::mount::MsFlags;

/// Where init keeps the rewritten files: on the `/run` tmpfs.
const DIR: &str = "/run/needy/account";

/// Makes the account files name `user`. A file that cannot be rewritten is
/// logged and skipped: the actor still boots, with the image's entries.
pub(super) fn apply(user: &Account<'_>) {
  for (name, rewrite) in FILES {
    let target = Path::new("/etc").join(name);
    let current = match std::fs::read_to_string(&target) {
      Ok(text) => text,
      // An image without shadow has no passwords to lock.
      Err(e) if e.kind() == std::io::ErrorKind::NotFound && name == "shadow" => continue,
      Err(e) => {
        tracing::warn!("cannot read {}: {e}", target.display());
        continue;
      }
    };
    let rewritten = rewrite(&current, user);
    if rewritten == current {
      continue;
    }
    if let Err(e) = replace(&target, name, &rewritten) {
      tracing::warn!("cannot give {} the entry of {}: {e}", target.display(), user.name);
    }
  }
}

/// Writes `text` to [`DIR`]`/name` and binds it over `target`, keeping the
/// target's mode (shadow is 0640 root:shadow, the others 0644).
fn replace(target: &Path, name: &str, text: &str) -> std::io::Result<()> {
  use std::os::unix::fs::PermissionsExt;
  std::fs::create_dir_all(DIR)?;
  let source = Path::new(DIR).join(name);
  std::fs::write(&source, text)?;
  let metadata = std::fs::metadata(target)?;
  std::fs::set_permissions(&source, std::fs::Permissions::from_mode(metadata.permissions().mode()))?;
  {
    use std::os::unix::fs::MetadataExt;
    std::os::unix::fs::chown(&source, Some(metadata.uid()), Some(metadata.gid()))?;
  }
  nix::mount::mount(Some(&source), target, None::<&str>, MsFlags::MS_BIND, None::<&str>)?;
  Ok(())
}
