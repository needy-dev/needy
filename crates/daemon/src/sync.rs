//! Sync mappings on the control host: a store copied into a per-run copy
//! the actor uses live, and merged back three-way when the run ends.
//!
//! Each selected `mode: sync` mapping of a run gets, under needy's data
//! directory, `sync/runs/RUN/NAME/` with the run's `seed` (the store as it
//! was copied), its per-run `copy` and the mapping as it was then
//! (`mapping.json`). The actor mounts the per-run copy through an
//! `sftp-server` on the control host ([`EXECUTOR`]), jailed to it. The copy
//! is on the host, so it survives any end of the actor.
//!
//! When the run ends, however it ends, each per-run copy is merged into its
//! store (`needy_common::merge`) under an exclusive lock on the store, one
//! run at a time per store; the run's copies of conflicts are kept under
//! `conflicts/RUN/NAME/`, outside the store. A merge that cannot run stays
//! pending: pending merges run when the daemon starts and before a run of
//! the same store is seeded. A seed and a per-run copy are removed once
//! merged.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use needy_api_server::control::{ControlError, ExecSpec};
use needy_common::merge::{self, Choice, Policy, Trees};
use needy_config::profile::{FileMapping, FileMode, OnConflict, Side};
use needy_core::event::{Event, FilesMerge, FilesMergePending, FilesSync, Store};
use needy_core::types::Origin;
use needy_core_actor::driver::HostMount;
use needy_trace::Dispatch;
use sha2::{Digest as _, Sha256};

use crate::Daemon;
use crate::pool::Instance;
use crate::run::Run;

/// The executor name an actor's mount of a per-run copy asks for: the
/// control host itself, which no profile names.
// NOTICE: a profile's executor named this could not be reached by mounts:
// the relay answers this name before the run's executors.
pub const EXECUTOR: &str = "needy:control-host";

/// Marks a run whose end merged, or tried to merge, its per-run copies, so
/// a run that is still listed (finished, not yet stopped) counts as pending.
const ENDED: &str = "ended";

/// A sync mapping as the run was seeded with it, kept next to its copies so
/// a pending merge does not depend on the run's profile.
#[derive(serde::Serialize, serde::Deserialize)]
struct Record {
  name: String,
  store: PathBuf,
  filter: Vec<String>,
  /// Patterns and `store`, `run` or `lines`, in order.
  on_conflict: Vec<(String, String)>,
}

impl Record {
  fn policy(&self) -> Result<Policy> {
    let on_conflict = self
      .on_conflict
      .iter()
      .map(|(pattern, choice)| {
        let choice = match choice.as_str() {
          "store" => Choice::Store,
          "run" => Choice::Run,
          "lines" => Choice::Lines,
          other => bail!("unknown on-conflict choice {other:?}"),
        };
        Ok((pattern.clone(), choice))
      })
      .collect::<Result<_>>()?;
    Ok(Policy {
      filter: self.filter.clone(),
      on_conflict,
    })
  }

  fn store(&self) -> Store {
    Store {
      path: self.store.display().to_string(),
      account: account(),
    }
  }
}

fn choice_name(choice: OnConflict) -> &'static str {
  match choice {
    OnConflict::Store => "store",
    OnConflict::Run => "run",
    OnConflict::Lines => "lines",
  }
}

/// The account this daemon reads and writes stores as.
fn account() -> String {
  // SAFETY: `geteuid` cannot fail.
  std::env::var("USER").unwrap_or_else(|_| format!("uid {}", unsafe { libc::geteuid() }))
}

/// The selected sync mappings of `run`: name, mapping, store and path in the
/// actor.
fn mappings(run: &Run) -> impl Iterator<Item = (&String, &FileMapping, &str, &str)> {
  run.profile.files.mappings.iter().filter(|(name, _)| crate::files::selected(run, name)).filter_map(|(name, mapping)| {
    match (mapping.mode, &mapping.from, &mapping.to) {
      (FileMode::Sync, Side::Host(store), Side::Actor(target)) => Some((name, mapping, store.as_str(), target.as_str())),
      _ => None,
    }
  })
}

fn runs_dir(daemon: &Daemon) -> PathBuf {
  daemon.config.data_dir.join("sync").join("runs")
}

fn run_dir(daemon: &Daemon, run: &str) -> PathBuf {
  runs_dir(daemon).join(run)
}

/// Where the per-run copy of mapping `name` of `run` is.
fn copy_dir(daemon: &Daemon, run: &str, name: &str) -> PathBuf {
  run_dir(daemon, run).join(name).join("copy")
}

/// Where needy keeps the run's copies of the entries its merge left in
/// conflict.
fn kept_dir(daemon: &Daemon, run: &str, name: &str) -> PathBuf {
  daemon.config.data_dir.join("conflicts").join(run).join(name)
}

/// Runs `f` holding the exclusive lock on `store`. The lock is a file per
/// store under needy's data directory, locked with `flock(2)`, so it
/// excludes this daemon's other merges and seeds of the store, also from
/// another task, and any other process that takes it.
// NOTICE: the lock is in this account's data directory rather than next
// to the store, so needy writes nothing into a store's parent; another
// account's daemon using the same store would not see it. The interim
// store is this account's (2026-11-17-images-and-run-data-spec.md).
fn locked<T>(daemon_data: &Path, store: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
  let locks = daemon_data.join("sync").join("locks");
  needy_api_server::create_private_dir(&locks).with_context(|| format!("create {}", locks.display()))?;
  let key = hex(&Sha256::digest(store.as_os_str().as_encoded_bytes()));
  let path = locks.join(format!("{key}.lock"));
  let file =
    std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path).with_context(|| format!("open {}", path.display()))?;
  file.lock().with_context(|| format!("lock the store {}", store.display()))?;
  let out = f();
  // Closing the file releases the lock.
  drop(file);
  out
}

fn hex(bytes: &[u8]) -> String {
  bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Checks the store of every sync mapping `run` uses, before the run
/// acquires anything: a store under needy's data directory is created when
/// missing; any other is the user's and must exist on the control host.
pub fn check(daemon: &Daemon, run: &Run) -> Result<()> {
  for (name, _, spec, _) in mappings(run) {
    let store = crate::files::host_path(run, spec);
    if store.starts_with(&daemon.config.data_dir) {
      needy_api_server::create_private_dir(&store).with_context(|| format!("sync {name}: create the store {}", store.display()))?;
    }
    if !store.is_dir() {
      bail!(
        "sync {name}: the store {} does not exist on the control host, where sync stores are; create it, or put it under {}",
        store.display(),
        daemon.config.data_dir.display()
      );
    }
  }
  Ok(())
}

/// Seeds every sync mapping `run` uses: runs the store's pending merges,
/// then copies the store, through the mapping's filter, into the run's seed
/// and per-run copy, under the store's lock.
pub async fn seed(daemon: &Daemon, run: &Run) -> Result<()> {
  for (name, mapping, spec, _) in mappings(run) {
    let store = crate::files::host_path(run, spec);
    drain_store(daemon, &store).await;
    let dir = run_dir(daemon, &run.id).join(name);
    needy_api_server::create_private_dir(&dir).with_context(|| format!("create {}", dir.display()))?;
    let record = Record {
      name: name.clone(),
      store: store.clone(),
      filter: mapping.filter.clone(),
      on_conflict: mapping.on_conflict.iter().map(|(pattern, choice)| (pattern.clone(), choice_name(*choice).to_string())).collect(),
    };
    let data = daemon.config.data_dir.clone();
    let entries = tokio::task::spawn_blocking(move || {
      locked(&data, &record.store, || {
        let archive = needy_common::archive::pack(&record.store, &record.filter)?;
        needy_common::archive::unpack(&archive, &dir.join("seed"), &[])?;
        let entries = needy_common::archive::unpack(&archive, &dir.join("copy"), &[])?;
        std::fs::write(dir.join("mapping.json"), serde_json::to_vec(&record)?)?;
        Ok(entries)
      })
    })
    .await?
    .with_context(|| format!("sync {name}: seed from the store {}", store.display()))?;
    run.emit(&Event::FilesSync(FilesSync {
      name: name.clone(),
      store: Store {
        path: store.display().to_string(),
        account: account(),
      },
      entries: entries as u64,
    }));
  }
  Ok(())
}

/// The per-run copies of `run`, as mounts of the control host into its
/// actor.
pub fn mounts(daemon: &Daemon, run: &Run) -> Vec<HostMount> {
  mappings(run)
    .filter_map(|(name, _, _, target)| {
      let copy = copy_dir(daemon, &run.id, name);
      copy.is_dir().then(|| HostMount {
        target: target.to_string(),
        executor: EXECUTOR.into(),
        source: copy.display().to_string(),
        writable: true,
        sftp_command: daemon.local.handle.sftp.clone(),
      })
    })
    .collect()
}

/// The instance an exec of `run` that names [`EXECUTOR`] runs on: this
/// machine, for a mount of one of the run's per-run copies only. `None`
/// when `spec` names another executor.
pub fn mount_instance(daemon: &Daemon, run: &Run, spec: &ExecSpec) -> Option<Result<Arc<Instance>, ControlError>> {
  if spec.executor != EXECUTOR {
    return None;
  }
  let mounted = spec.origin == Origin::ActorMount
    && spec.argv == daemon.local.handle.sftp
    && mounts(daemon, run).iter().any(|m| m.source.trim_end_matches('/') == spec.cwd.trim_end_matches('/'));
  Some(if mounted {
    Ok(daemon.local.clone())
  } else {
    Err(ControlError::denied(format!("{EXECUTOR} serves only the mounts of the run's per-run copies")))
  })
}

/// Merges `run`'s per-run copies into their stores, as its end does.
/// Each merge that fails stays pending and is recorded.
pub async fn end(daemon: &Daemon, run: &Run) {
  let dir = run_dir(daemon, &run.id);
  if !dir.is_dir() {
    return;
  }
  // The actor is gone; the `sftp-server`s its mounts ran on this machine
  // end too, so nothing writes a per-run copy while it is merged.
  daemon.end_on_control_host(&run.id, &daemon.local).await;
  if let Err(e) = std::fs::write(dir.join(ENDED), "") {
    tracing::warn!("run {}: cannot mark its per-run copies ended: {e}", run.id);
  }
  merge_run(daemon, &run.id, None, &run.trace).await;
}

/// Runs every pending merge, as the daemon does when it starts.
pub async fn drain(daemon: &Daemon) {
  for id in pending(daemon) {
    merge_pending(daemon, &id, None).await;
  }
}

/// Runs the pending merges into `store`, before a run of it is seeded.
async fn drain_store(daemon: &Daemon, store: &Path) {
  for id in pending(daemon) {
    merge_pending(daemon, &id, Some(store)).await;
  }
}

/// The runs with a per-run copy left to merge: those that ended, and those
/// this daemon no longer lists, whose end a previous daemon did not see.
fn pending(daemon: &Daemon) -> Vec<String> {
  let Ok(entries) = std::fs::read_dir(runs_dir(daemon)) else {
    return Vec::new();
  };
  let mut ids: Vec<String> = entries
    .filter_map(|e| e.ok())
    .filter(|e| e.path().is_dir())
    .map(|e| e.file_name().to_string_lossy().into_owned())
    .filter(|id| run_dir(daemon, id).join(ENDED).exists() || daemon.runs.exact(id).is_none())
    .collect();
  ids.sort();
  ids
}

/// Merges a pending run's copies, recorded in its trace: the live run's, or
/// for a run this daemon no longer lists, its stored trace continued.
async fn merge_pending(daemon: &Daemon, id: &str, store: Option<&Path>) {
  if let Some(run) = daemon.runs.exact(id) {
    merge_run(daemon, id, store, &run.trace).await;
    return;
  }
  let next_seq = match needy_trace::sink::sqlite::load(&daemon.config.state_dir.join("trace.db"), id).await {
    Ok(loaded) => loaded.events.last().map_or(0, |event| event.seq + 1),
    Err(e) => {
      tracing::warn!("run {id}: cannot read its trace to record a pending merge: {e:#}");
      0
    }
  };
  // NOTICE: the default sinks: the run's profile chose its sinks, but the
  // report reads `trace.db`, which they include unless the profile left it
  // out.
  let trace = daemon.run_trace_at(id, None, next_seq);
  merge_run(daemon, id, store, &trace).await;
  trace.flush().await;
}

/// Merges the per-run copies of run `id` (those into `store` only, when
/// given) and records each merge, or why it stays pending, in `trace`.
async fn merge_run(daemon: &Daemon, id: &str, store: Option<&Path>, trace: &Dispatch) {
  let dir = run_dir(daemon, id);
  let Ok(entries) = std::fs::read_dir(&dir) else {
    return;
  };
  let mut names: Vec<String> =
    entries.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
  names.sort();
  for name in names {
    let mapping_dir = dir.join(&name);
    let record: Record =
      match std::fs::read(mapping_dir.join("mapping.json")).map_err(anyhow::Error::from).and_then(|b| Ok(serde_json::from_slice(&b)?)) {
        Ok(record) => record,
        // A seed that failed before it finished left nothing the actor used.
        Err(e) => {
          tracing::warn!("run {id}: sync {name} was not seeded ({e:#}); removing it");
          remove(&mapping_dir);
          continue;
        }
      };
    if store.is_some_and(|store| store != record.store) {
      continue;
    }
    let kept = kept_dir(daemon, id, &name);
    let data = daemon.config.data_dir.clone();
    let merged = {
      let (mapping_dir, kept) = (mapping_dir.clone(), kept.clone());
      let store = record.store.clone();
      let policy = record.policy();
      tokio::task::spawn_blocking(move || {
        let policy = policy?;
        locked(&data, &store, || {
          let trees = Trees {
            seed: &mapping_dir.join("seed"),
            run: &mapping_dir.join("copy"),
            store: &store,
          };
          merge::merge(trees, &kept, &policy)
        })
      })
      .await
      .map_err(anyhow::Error::from)
      .and_then(|merged| merged)
    };
    let event = match merged {
      Ok(outcome) => {
        remove(&mapping_dir);
        let conflicts: Vec<String> = outcome.conflicts().map(str::to_string).collect();
        let count = |pick: fn(&merge::Action) -> bool| outcome.changes.iter().filter(|c| pick(&c.action)).count() as u64;
        Event::FilesMerge(FilesMerge {
          name: record.name.clone(),
          store: record.store(),
          taken: count(|a| matches!(a, merge::Action::Taken | merge::Action::Removed)),
          resolved: count(|a| matches!(a, merge::Action::Resolved(_))),
          kept: (!conflicts.is_empty()).then(|| kept.display().to_string()),
          conflicts,
        })
      }
      Err(e) => {
        tracing::warn!("run {id}: sync {name}: the merge stays pending: {e:#}");
        Event::FilesMergePending(FilesMergePending {
          name: record.name.clone(),
          store: record.store(),
          reason: format!("{e:#}"),
        })
      }
    };
    crate::run::emit(trace, &event);
  }
  // The run's directory goes once nothing in it is left to merge.
  let left = std::fs::read_dir(&dir).map(|entries| entries.filter_map(|e| e.ok()).any(|e| e.path().is_dir())).unwrap_or(false);
  if !left {
    remove(&dir);
  }
}

fn remove(dir: &Path) {
  if let Err(e) = std::fs::remove_dir_all(dir)
    && e.kind() != std::io::ErrorKind::NotFound
  {
    tracing::warn!("cannot remove {}: {e}", dir.display());
  }
}

#[cfg(test)]
#[path = "sync_test.rs"]
mod tests;
