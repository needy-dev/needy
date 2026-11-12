//! Copy-in and mirror-out on the actor's side: archives fetched from and
//! pushed to the daemon, owned by the user once unpacked.

use std::path::{Path, PathBuf};

use anyhow::Result;
use needy_api_client::Client;
use needy_api_proto::v1::{FetchFilesRequest, PushFilesRequest};

/// Fetches a copy-in mapping from the host and unpacks it at `path`, owned
/// by the user, as are the directories above it that it creates under the
/// user's `home`.
pub(super) async fn copy_in(client: &Client, name: &str, path: &str, home: &str, uid: u32, gid: u32) -> Result<()> {
  let mut stream =
    client.guest().fetch_files(FetchFilesRequest { name: name.into() }).await.map_err(needy_api_client::Error::from)?.into_inner();
  let mut archive = Vec::new();
  while let Some(chunk) = stream.message().await.map_err(needy_api_client::Error::from)? {
    archive.extend(chunk.data);
  }
  place(&archive, Path::new(path), home, uid, gid)
}

/// Unpacks a copy-in `archive` at `root` and gives its tree to the user.
/// The missing directories from the user's `home` down to `root` are
/// created first, as the user's; the unpack creates those of a `root`
/// outside the home, as before.
fn place(archive: &[u8], root: &Path, home: &str, uid: u32, gid: u32) -> Result<()> {
  create_user_dirs(root, home, uid, gid);
  needy_common::archive::unpack(archive, root, &[])?;
  chown_tree(root, uid, gid);
  Ok(())
}

/// Creates the driving program's working directory `cwd` when it is under
/// the user's `home` and missing: an `actor.workspace` without `from`.
pub(super) fn working_directory(cwd: &str, home: &str, uid: u32, gid: u32) {
  create_user_dirs(Path::new(cwd), home, uid, gid);
}

/// Creates `path` and each missing directory between the user's `home` and
/// it. Each directory it creates belongs to the user; one that exists keeps
/// its owner, and a path outside the home is left alone, since the root
/// file system is read-only.
fn create_user_dirs(path: &Path, home: &str, uid: u32, gid: u32) {
  let Ok(relative) = path.strip_prefix(home) else {
    return;
  };
  let mut dir = PathBuf::from(home);
  for part in relative.components() {
    dir.push(part);
    if dir.is_dir() {
      continue;
    }
    if let Err(e) = std::fs::create_dir(&dir) {
      tracing::warn!("cannot create {}: {e}", dir.display());
      return;
    }
    if let Err(e) = std::os::unix::fs::lchown(&dir, Some(uid), Some(gid)) {
      tracing::warn!("cannot give {} to {uid}:{gid}: {e}", dir.display());
    }
  }
}

/// Gives a tree to the user, without following symbolic links. A file that
/// cannot be changed is logged and keeps its owner.
fn chown_tree(path: &Path, uid: u32, gid: u32) {
  if let Err(e) = std::os::unix::fs::lchown(path, Some(uid), Some(gid)) {
    tracing::warn!("cannot give {} to {uid}:{gid}: {e}", path.display());
  }
  if path.is_symlink() || !path.is_dir() {
    return;
  }
  match std::fs::read_dir(path) {
    Ok(entries) => {
      for entry in entries {
        match entry {
          Ok(entry) => chown_tree(&entry.path(), uid, gid),
          Err(e) => tracing::warn!("cannot list {}: {e}", path.display()),
        }
      }
    }
    Err(e) => tracing::warn!("cannot list {}: {e}", path.display()),
  }
}

/// Packs `path` and sends it to the host for a mirror-out mapping. A
/// directory the driving program never created sends nothing.
pub(super) async fn mirror_out(client: &Client, name: &str, path: &str) -> Result<()> {
  let root = PathBuf::from(path);
  if !root.exists() {
    return Ok(());
  }
  let archive = needy_common::archive::pack(&root, &[])?;
  // NOTICE: 1 MiB chunks stay under gRPC's default 4 MiB message limit.
  let mut messages: Vec<PushFilesRequest> = archive
    .chunks(1 << 20)
    .enumerate()
    .map(|(i, chunk)| PushFilesRequest {
      name: if i == 0 { name.into() } else { String::new() },
      data: chunk.to_vec(),
    })
    .collect();
  if messages.is_empty() {
    messages.push(PushFilesRequest {
      name: name.into(),
      data: Vec::new(),
    });
  }
  client.guest().push_files(tokio_stream::iter(messages)).await.map_err(needy_api_client::Error::from)?;
  Ok(())
}

#[cfg(test)]
#[path = "files_test.rs"]
mod tests;
