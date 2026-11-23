//! Copy-in and mirror-out on the run's side: host directories packed
//! into an executor or handed to the actor at start, and executor or actor
//! directories unpacked on the host at stop. Archives travel as tar over
//! exec streams (`api_client::files`).

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use needy_api_client::files::{self, Target};
use needy_api_client::proto::v1::Origin as WireOrigin;
use needy_api_server::control::ControlError;
use needy_config::profile::{FileMode, Side};
use needy_core::event::{Event, FileEnd, FilesCopyIn, FilesFail, FilesMirrorOut, Transfer};

use crate::failed;
use crate::run::Run;

/// Operator policy restricting `host:` file mappings on the control host.
#[derive(Debug, Clone, Default)]
pub struct HostFilesPolicy {
  pub disabled: bool,
  pub root: Option<PathBuf>,
}

impl HostFilesPolicy {
  pub fn from_config(config: &crate::Config) -> Self {
    Self {
      disabled: config.disable_host_files,
      root: config.host_files_root.clone(),
    }
  }
}

fn reject_if_disabled(policy: &HostFilesPolicy) -> Result<()> {
  if policy.disabled {
    anyhow::bail!("host: file mappings are disabled on this daemon (--disable-host-files)");
  }
  Ok(())
}

fn canonical_root(root: &Path) -> Result<PathBuf> {
  std::fs::canonicalize(root).with_context(|| format!("host-files-root {} does not exist or cannot be canonicalized", root.display()))
}

/// Resolves `path` (which may not exist yet) by canonicalizing its deepest
/// existing ancestor and applying the remaining path components, rejecting
/// dangling symlinks and any path that escapes `root`.
fn resolve_within_root(path: &Path, root: &Path) -> Result<PathBuf> {
  let canon_root = canonical_root(root)?;
  let mut existing = path;
  let mut suffix = Vec::new();
  while std::fs::symlink_metadata(existing).is_err() {
    let Some(name) = existing.file_name() else {
      break;
    };
    suffix.push(name.to_os_string());
    let Some(parent) = existing.parent() else {
      break;
    };
    existing = parent;
  }
  let mut resolved = std::fs::canonicalize(existing)
    .with_context(|| format!("cannot canonicalize host path {} (at {})", path.display(), existing.display()))?;
  for part in suffix.into_iter().rev() {
    let part_path = Path::new(&part);
    for component in part_path.components() {
      match component {
        Component::Normal(seg) => resolved.push(seg),
        Component::CurDir => {}
        Component::ParentDir => {
          resolved.pop();
        }
        Component::RootDir | Component::Prefix(_) => {}
      }
    }
  }
  if !resolved.starts_with(&canon_root) {
    anyhow::bail!("host path {} (resolved to {}) is outside host-files-root {}", path.display(), resolved.display(), canon_root.display());
  }
  Ok(resolved)
}

/// A profile path on the host: `{run}` is the run's ID, `~` the
/// user's home directory, and a relative path resolves against the
/// profile's directory.
pub(crate) fn host_path(run: &Run, path: &str) -> PathBuf {
  let path = path.replace("{run}", &run.id);
  let path = needy_core_executor::driver::expand_home(&path, &needy_core_executor::driver::home());
  if Path::new(&path).is_absolute() {
    PathBuf::from(path)
  } else {
    run.profile.base_dir.join(path)
  }
}

/// The directory under which mirror-out may create the `host:` path `spec`:
/// the profile's directory for a relative path, the home directory for one
/// under `~`, and the parent of an absolute path.
fn anchor(run: &Run, spec: &str) -> PathBuf {
  if spec.starts_with('~') {
    return PathBuf::from(std::env::var("HOME").unwrap_or_default());
  }
  let path = host_path(run, spec);
  if Path::new(spec).is_absolute() {
    path.parent().map(Path::to_path_buf).unwrap_or(path)
  } else {
    run.profile.base_dir.clone()
  }
}

// TODO(runner-files): `host:` paths are resolved on the control host, not
// on the client that sent the profile; resolving them on the client waits
// for a client-side file transfer. Until then, a path the control host
// does not have fails the run instead of copying nothing or creating it.

/// Checks that the copy-in source `spec` can be read here, on the control
/// host, and returns its path.
fn readable(run: &Run, spec: &str, policy: &HostFilesPolicy) -> Result<PathBuf> {
  reject_if_disabled(policy)?;
  let path = host_path(run, spec);
  // Enforce the jail boundary before probing or opening the target path so an
  // out-of-root path cannot block on a FIFO or leak host path existence.
  let checked = match &policy.root {
    Some(root) => resolve_within_root(&path, root)?,
    None => path,
  };
  let read = match std::fs::metadata(&checked) {
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
      anyhow::bail!("host path {} does not exist on the control host, where `host:` paths are read", checked.display())
    }
    Err(e) => Err(e),
    Ok(meta) if meta.is_dir() => std::fs::read_dir(&checked).map(drop),
    Ok(_) => std::fs::File::open(&checked).map(drop),
  };
  read.with_context(|| format!("cannot read host path {} on the control host", checked.display()))?;
  Ok(checked)
}

/// Checks that the mirror-out destination `spec` can be created here, on
/// the control host, and returns its path. It may be created only under a
/// directory the control host has (see [`anchor`]), so a path of another
/// machine is refused instead of being created as a new tree.
fn destination(run: &Run, spec: &str, policy: &HostFilesPolicy) -> Result<PathBuf> {
  reject_if_disabled(policy)?;
  let path = host_path(run, spec);
  // Enforce the jail boundary before probing the target or its anchor so an
  // out-of-root path cannot leak host path existence.
  let checked = match &policy.root {
    Some(root) => resolve_within_root(&path, root)?,
    None => path,
  };
  if checked.is_dir() {
    return Ok(checked);
  }
  if checked.exists() {
    anyhow::bail!("host path {} on the control host is not a directory", checked.display());
  }
  let anchor = anchor(run, spec);
  if !anchor.is_dir() {
    anyhow::bail!(
      "cannot create host path {}: {} does not exist on the control host, where `host:` paths are written",
      checked.display(),
      anchor.display()
    );
  }
  Ok(checked)
}

/// Checks the `host:` side of every mapping the run uses, before the run
/// acquires anything: a copy-in source must be readable and a mirror-out
/// destination creatable on the control host, and all `host:` mappings must
/// satisfy `policy`.
pub fn check_host_paths(run: &Run, policy: &HostFilesPolicy) -> Result<()> {
  for (name, mapping) in &run.profile.files.mappings {
    if !selected(run, name) {
      continue;
    }
    match (mapping.mode, &mapping.from, &mapping.to) {
      (FileMode::CopyIn, Side::Host(src), _) => drop(readable(run, src, policy).with_context(|| format!("copy-in {name}"))?),
      (FileMode::MirrorOut, _, Side::Host(dst)) => drop(destination(run, dst, policy).with_context(|| format!("mirror-out {name}"))?),
      (FileMode::Sync, Side::Host(store), _) => {
        reject_if_disabled(policy).with_context(|| format!("sync {name}"))?;
        if let Some(root) = &policy.root {
          let path = host_path(run, store);
          resolve_within_root(&path, root).with_context(|| format!("sync {name}"))?;
        }
      }
      _ => {}
    }
  }
  Ok(())
}

/// Records that the transfer of mapping `name` failed, for the report.
fn record_failure(run: &Run, name: &str, transfer: Transfer, error: &anyhow::Error) {
  run.emit(&Event::FilesFail(FilesFail {
    name: name.to_string(),
    transfer,
    reason: format!("{error:#}"),
  }));
}

/// The selected executor receives file commands directly, with the owning
/// invocation retained for its admission checks and process cleanup.
fn target(run: &Run, executor: &str) -> Target {
  Target {
    executor: executor.to_string(),
    run: run.id.clone(),
    runner: String::new(),
    origin: WireOrigin::Files,
  }
}

/// Whether the run uses mapping `name`: all mappings when the profile
/// names none in `run_files`.
pub fn selected(run: &Run, name: &str) -> bool {
  let names = &run.profile.run_files;
  names.is_empty() || names.iter().any(|n| n == name)
}

/// Packs each host-to-executor copy-in mapping into its executor.
pub async fn copy_in(run: &Run, policy: &HostFilesPolicy) -> Result<()> {
  for (name, mapping) in &run.profile.files.mappings {
    if !selected(run, name) {
      continue;
    }
    let (FileMode::CopyIn, Side::Host(src), Side::Executor { name: exec, path }) = (mapping.mode, &mapping.from, &mapping.to) else {
      continue;
    };
    let Some(instance) = run.executor(exec) else {
      continue;
    };
    let path = &instance.expand_home(path);
    let root = readable(run, src, policy).with_context(|| format!("copy-in {name}"))?;
    let filter = mapping.filter.clone();
    let archive = tokio::task::spawn_blocking(move || needy_common::archive::pack(&root, &filter)).await??;
    let size = archive.len();
    files::push(&*instance.handle.executor, &target(run, exec), archive, path).await.with_context(|| format!("copy-in {name}"))?;
    run.emit(&Event::FilesCopyIn(FilesCopyIn {
      name: name.clone(),
      to: FileEnd::Executor {
        name: exec.clone(),
        path: path.clone(),
      },
      bytes: size as u64,
    }));
  }
  Ok(())
}

/// Unpacks each executor-to-host mirror-out mapping on the host, through its
/// filter. A mapping that fails is recorded (`files.fail`) and logged, and
/// the others still run.
// TODO(run-ls): a failed mirror-out is a report finding but does not show in
// `run ls`, whose rows carry the driving program's exit or the run's own
// failure; a column for it changes the `Run` message and waits for the owner.
pub async fn mirror_out(run: &Run, policy: &HostFilesPolicy) {
  for (name, mapping) in &run.profile.files.mappings {
    if !selected(run, name) {
      continue;
    }
    let (FileMode::MirrorOut, Side::Executor { name: exec, path }, Side::Host(dst)) = (mapping.mode, &mapping.from, &mapping.to) else {
      continue;
    };
    let Some(instance) = run.executor(exec) else {
      continue;
    };
    let path = &instance.expand_home(path);
    let mirrored = async {
      let destination = destination(run, dst, policy)?;
      let archive = files::pull(&*instance.handle.executor, &target(run, exec), path, ".").await?;
      let size = archive.len();
      let filter = mapping.filter.clone();
      tokio::task::spawn_blocking(move || needy_common::archive::unpack(&archive, &destination, &filter)).await??;
      anyhow::Ok(size)
    };
    match mirrored.await {
      Ok(size) => run.emit(&Event::FilesMirrorOut(FilesMirrorOut {
        name: name.clone(),
        from: FileEnd::Executor {
          name: exec.clone(),
          path: path.clone(),
        },
        bytes: size as u64,
        entries: None,
      })),
      Err(e) => {
        tracing::warn!("run {}: mirror-out {name}: {e:#}", run.id);
        record_failure(run, name, Transfer::MirrorOut, &e);
      }
    }
  }
}

/// The archive of copy-in mapping `name` for the actor.
// TODO(run-files): `fetch` and `push` serve any mapping the actor names,
// while the actor is only told the ones `run_files` selects (`guest.rs`);
// refusing an unselected name here is a guest API change that waits for the
// owner.
pub async fn fetch(run: &Run, name: &str, policy: &HostFilesPolicy) -> Result<Vec<u8>, ControlError> {
  let mapping = run
    .profile
    .files
    .mappings
    .get(name)
    .filter(|m| m.mode == FileMode::CopyIn)
    .ok_or_else(|| ControlError::NotFound("no such copy-in mapping".into()))?;
  let Side::Host(src) = &mapping.from else {
    return Err(ControlError::Invalid("copy-in must come from the host".into()));
  };
  let filter = mapping.filter.clone();
  let packed = match readable(run, src, policy) {
    Ok(root) => tokio::task::spawn_blocking(move || needy_common::archive::pack(&root, &filter))
      .await
      .map_err(|e| ControlError::Internal(e.to_string()))?,
    Err(e) => Err(e),
  };
  let archive = packed.inspect_err(|e| record_failure(run, name, Transfer::CopyIn, e)).map_err(failed)?;
  run.emit(&Event::FilesCopyIn(FilesCopyIn {
    name: name.to_string(),
    to: FileEnd::Actor,
    bytes: archive.len() as u64,
  }));
  Ok(archive)
}

/// Unpacks the actor's archive of mirror-out mapping `name` on the host,
/// through the mapping's filter.
pub async fn push(run: &Run, name: &str, archive: Vec<u8>, policy: &HostFilesPolicy) -> Result<(), ControlError> {
  let mapping = run
    .profile
    .files
    .mappings
    .get(name)
    .filter(|m| m.mode == FileMode::MirrorOut)
    .ok_or_else(|| ControlError::NotFound("no such mirror-out mapping".into()))?;
  let Side::Host(dst) = &mapping.to else {
    return Err(ControlError::Invalid("mirror-out must go to the host".into()));
  };
  let size = archive.len();
  // The filter applies here, not in the guest, so the guest cannot bypass
  // it and the rules live where copy-in applies them.
  let filter = mapping.filter.clone();
  let unpacked = match destination(run, dst, policy) {
    Ok(destination) => tokio::task::spawn_blocking(move || needy_common::archive::unpack(&archive, &destination, &filter))
      .await
      .map_err(|e| ControlError::Internal(e.to_string()))?,
    Err(e) => Err(e),
  };
  let count = unpacked.inspect_err(|e| record_failure(run, name, Transfer::MirrorOut, e)).map_err(failed)?;
  run.emit(&Event::FilesMirrorOut(FilesMirrorOut {
    name: name.to_string(),
    from: FileEnd::Actor,
    bytes: size as u64,
    entries: Some(count as u64),
  }));
  Ok(())
}

#[cfg(all(test, unix))]
#[path = "files_test.rs"]
mod tests;
