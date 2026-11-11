//! Copy-in and mirror-out on the run's side: host directories packed
//! into an executor or handed to the actor at start, and executor or actor
//! directories unpacked on the host at stop. Archives travel as tar over
//! exec streams (`api_client::files`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use needy_api_client::files::{self, Target};
use needy_api_client::proto::v1::Origin as WireOrigin;
use needy_api_server::control::ControlError;
use needy_config::profile::{FileMode, Side};
use needy_core::event::{Event, FileEnd, FilesCopyIn, FilesFail, FilesMirrorOut, Transfer};

use crate::failed;
use crate::run::Run;

/// A profile path on the host: `{run}` is the run's ID, `~` the
/// user's home directory, and a relative path resolves against the
/// profile's directory.
fn host_path(run: &Run, path: &str) -> PathBuf {
  let path = path.replace("{run}", &run.id);
  let home = std::env::var("HOME").unwrap_or_default();
  let path = path.replacen('~', &home, 1);
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
fn readable(run: &Run, spec: &str) -> Result<PathBuf> {
  let path = host_path(run, spec);
  let read = match std::fs::metadata(&path) {
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
      anyhow::bail!("host path {} does not exist on the control host, where `host:` paths are read", path.display())
    }
    Err(e) => Err(e),
    Ok(meta) if meta.is_dir() => std::fs::read_dir(&path).map(drop),
    Ok(_) => std::fs::File::open(&path).map(drop),
  };
  read.with_context(|| format!("cannot read host path {} on the control host", path.display()))?;
  Ok(path)
}

/// Checks that the mirror-out destination `spec` can be created here, on
/// the control host, and returns its path. It may be created only under a
/// directory the control host has (see [`anchor`]), so a path of another
/// machine is refused instead of being created as a new tree.
fn destination(run: &Run, spec: &str) -> Result<PathBuf> {
  let path = host_path(run, spec);
  if path.is_dir() {
    return Ok(path);
  }
  if path.exists() {
    anyhow::bail!("host path {} on the control host is not a directory", path.display());
  }
  let anchor = anchor(run, spec);
  if !anchor.is_dir() {
    anyhow::bail!(
      "cannot create host path {}: {} does not exist on the control host, where `host:` paths are written",
      path.display(),
      anchor.display()
    );
  }
  Ok(path)
}

/// Checks the `host:` side of every mapping the run uses, before the run
/// acquires anything: a copy-in source must be readable and a mirror-out
/// destination creatable on the control host.
pub fn check_host_paths(run: &Run) -> Result<()> {
  for (name, mapping) in &run.profile.files.mappings {
    if !selected(run, name) {
      continue;
    }
    match (mapping.mode, &mapping.from, &mapping.to) {
      (FileMode::CopyIn, Side::Host(src), _) => drop(readable(run, src).with_context(|| format!("copy-in {name}"))?),
      (FileMode::MirrorOut, _, Side::Host(dst)) => drop(destination(run, dst).with_context(|| format!("mirror-out {name}"))?),
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

/// Where the daemon's own `tar` runs: straight on the instance's executor,
/// outside any run.
fn target() -> Target {
  Target {
    executor: String::new(),
    run: String::new(),
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
pub async fn copy_in(run: &Run) -> Result<()> {
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
    let root = host_path(run, src);
    let filter = mapping.filter.clone();
    let archive = tokio::task::spawn_blocking(move || needy_common::archive::pack(&root, &filter)).await??;
    let size = archive.len();
    files::push(&*instance.handle.executor, &target(), archive, path).await.with_context(|| format!("copy-in {name}"))?;
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

/// Unpacks each executor-to-host mirror-out mapping on the host. A mapping
/// that fails is recorded (`files.fail`) and logged, and the others still
/// run.
// TODO(run-ls): a failed mirror-out is a report finding but does not show in
// `run ls`, whose rows carry the driving program's exit or the run's own
// failure; a column for it changes the `Run` message and waits for the owner.
pub async fn mirror_out(run: &Run) {
  for (name, mapping) in &run.profile.files.mappings {
    let (FileMode::MirrorOut, Side::Executor { name: exec, path }, Side::Host(dst)) = (mapping.mode, &mapping.from, &mapping.to) else {
      continue;
    };
    let Some(instance) = run.executor(exec) else {
      continue;
    };
    let mirrored = async {
      let destination = destination(run, dst)?;
      let archive = files::pull(&*instance.handle.executor, &target(), path, ".").await?;
      let size = archive.len();
      tokio::task::spawn_blocking(move || needy_common::archive::unpack(&archive, &destination)).await??;
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
pub async fn fetch(run: &Run, name: &str) -> Result<Vec<u8>, ControlError> {
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
  let packed = match readable(run, src) {
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

/// Unpacks the actor's archive of mirror-out mapping `name` on the host.
pub async fn push(run: &Run, name: &str, archive: Vec<u8>) -> Result<(), ControlError> {
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
  let unpacked = match destination(run, dst) {
    Ok(destination) => tokio::task::spawn_blocking(move || needy_common::archive::unpack(&archive, &destination))
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
