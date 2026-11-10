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
use needy_core::event::{Event, FileEnd, FilesCopyIn, FilesMirrorOut};

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

/// Unpacks each executor-to-host mirror-out mapping on the host.
pub async fn mirror_out(run: &Run) -> Result<()> {
  for (name, mapping) in &run.profile.files.mappings {
    let (FileMode::MirrorOut, Side::Executor { name: exec, path }, Side::Host(dst)) = (mapping.mode, &mapping.from, &mapping.to) else {
      continue;
    };
    let Some(instance) = run.executor(exec) else {
      continue;
    };
    let archive = files::pull(&*instance.handle.executor, &target(), path, ".").await.with_context(|| format!("mirror-out {name}"))?;
    let destination = host_path(run, dst);
    let size = archive.len();
    tokio::task::spawn_blocking(move || needy_common::archive::unpack(&archive, &destination)).await??;
    run.emit(&Event::FilesMirrorOut(FilesMirrorOut {
      name: name.clone(),
      from: FileEnd::Executor {
        name: exec.clone(),
        path: path.clone(),
      },
      bytes: size as u64,
      entries: None,
    }));
  }
  Ok(())
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
  let root = host_path(run, src);
  let filter = mapping.filter.clone();
  let archive = tokio::task::spawn_blocking(move || needy_common::archive::pack(&root, &filter))
    .await
    .map_err(|e| ControlError::Internal(e.to_string()))?
    .map_err(failed)?;
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
  let destination = host_path(run, dst);
  let size = archive.len();
  let count = tokio::task::spawn_blocking(move || needy_common::archive::unpack(&archive, &destination))
    .await
    .map_err(|e| ControlError::Internal(e.to_string()))?
    .map_err(failed)?;
  run.emit(&Event::FilesMirrorOut(FilesMirrorOut {
    name: name.to_string(),
    from: FileEnd::Actor,
    bytes: size as u64,
    entries: Some(count as u64),
  }));
  Ok(())
}
