//! Copy-in and mirror-out on the actor's side: archives fetched from and
//! pushed to the daemon, owned by the user once unpacked.

use std::path::{Path, PathBuf};

use anyhow::Result;
use needy_api_client::Client;
use needy_api_proto::v1::{FetchFilesRequest, PushFilesRequest};

/// Fetches a copy-in mapping from the host and unpacks it at `path`, owned
/// by the user.
pub(super) async fn copy_in(client: &Client, name: &str, path: &str, uid: u32, gid: u32) -> Result<()> {
  let mut stream =
    client.guest().fetch_files(FetchFilesRequest { name: name.into() }).await.map_err(needy_api_client::Error::from)?.into_inner();
  let mut archive = Vec::new();
  while let Some(chunk) = stream.message().await.map_err(needy_api_client::Error::from)? {
    archive.extend(chunk.data);
  }
  let root = PathBuf::from(path);
  needy_common::archive::unpack(&archive, &root)?;
  chown_tree(&root, uid, gid);
  Ok(())
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
