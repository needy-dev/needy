//! The profile's user in a container's account files, set up from the
//! host for both `linux-container` drivers: the executor's container runs
//! as the user from its start, and the actor's has a read-only root and an
//! init that may not mount, so neither can rewrite `/etc/passwd` itself.
//!
//! The image's `/etc/passwd`, `/etc/group` and `/etc/shadow` are read
//! through the engine from a container that is created and never started,
//! rewritten for the user ([`needy_common::account`]) into a host
//! directory, and bind-mounted read-only over the originals. `whoami`, `id`,
//! `getent passwd` and the `HOME` the engine sets from passwd then name the
//! profile's user, which owns the workspace on both sides.

use std::io::Read;
use std::path::Path;

use needy_common::account::{Account, FILES};

use crate::engine::{Engine, Error, models::ContainerCreateBody};

/// Writes the account files of `image`, rewritten for `user`, into `dir`
/// and returns their binds (`SOURCE:/etc/NAME:ro`). `scratch` names the
/// container the files are read from, removed again before this returns. An
/// image without a file gets no bind for it; a missing image is pulled, as
/// `docker run` does.
pub async fn binds(engine: &dyn Engine, image: &str, scratch: &str, dir: &Path, user: &Account<'_>) -> Result<Vec<String>, Error> {
  let body = ContainerCreateBody {
    image: Some(image.to_string()),
    // Never started; a command only satisfies an image without one.
    entrypoint: Some(vec!["/bin/true".into()]),
    cmd: Some(Vec::new()),
    ..Default::default()
  };
  // One left by an earlier daemon would block the name.
  engine.remove(scratch).await?;
  match engine.create(scratch, body.clone()).await {
    Err(e) if e.is_not_found() => {
      engine.pull(image).await?;
      engine.create(scratch, body).await?;
    }
    created => created?,
  }
  let read = read_files(engine, scratch).await;
  if let Err(e) = engine.remove(scratch).await {
    tracing::warn!("cannot remove {scratch}: {e}");
  }
  let mut binds = Vec::new();
  for ((name, rewrite), text) in FILES.iter().zip(read?) {
    let Some(text) = text else {
      continue;
    };
    let source = dir.join(name);
    write(&source, &rewrite(&text, user), *name == "shadow")
      .map_err(|e| Error::Refused(format!("cannot write {}: {e}", source.display())))?;
    binds.push(format!("{}:/etc/{name}:ro", source.display()));
  }
  Ok(binds)
}

/// The text of each account file in the container `name`, or `None`.
async fn read_files(engine: &dyn Engine, name: &str) -> Result<Vec<Option<String>>, Error> {
  let mut files = Vec::new();
  for (file, _) in FILES {
    let path = format!("/etc/{file}");
    let text = match engine.download(name, &path).await? {
      Some(archive) => Some(first_file(&archive).map_err(|e| Error::Refused(format!("cannot read {path} from {name}: {e}")))?),
      None => None,
    };
    files.push(text);
  }
  Ok(files)
}

/// The content of the first entry of a tar `archive`: the engine archives
/// one file as one entry.
// Reference: https://github.com/moby/moby/blob/v28.5.1/api/swagger.yaml#L7869-L7905
// (`GET /containers/{id}/archive`: "a tar archive of a resource in the
// filesystem of container id").
fn first_file(archive: &[u8]) -> std::io::Result<String> {
  let mut archive = tar::Archive::new(archive);
  let mut entry = archive.entries()?.next().ok_or_else(|| std::io::Error::other("an empty archive"))??;
  let mut text = String::new();
  entry.read_to_string(&mut text)?;
  Ok(text)
}

/// Writes an account file the container reads: shadow 0640, the others
/// 0644, as in the image. The container's root reads shadow through its
/// `CAP_DAC_OVERRIDE`.
fn write(path: &Path, text: &str, secret: bool) -> std::io::Result<()> {
  if let Some(dir) = path.parent() {
    std::fs::create_dir_all(dir)?;
  }
  std::fs::write(path, text)?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(if secret { 0o640 } else { 0o644 }))?;
  }
  #[cfg(not(unix))]
  let _ = secret;
  Ok(())
}

#[cfg(test)]
#[path = "account_test.rs"]
mod tests;
