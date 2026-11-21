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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use needy_api_server::control::{ControlError, ExecSpec, GuestSession, UpperLayer};
use needy_common::merge::{self, Choice, DriverClaim, Policy, Trees};
use needy_common::process::Exit;
use needy_config::profile::{ActorDriver, FileMode, OnConflict, Side, User};
use needy_core::event::{Event, FilesMerge, FilesMergePending, FilesSync, Store};
use needy_core::image::Digest;
use needy_core::types::Origin;
use needy_core_actor::driver::HostMount;
use needy_trace::Dispatch;
use sha2::{Digest as _, Sha256};

use crate::Daemon;
use crate::image::SessionSpec;
use crate::pool::Instance;
use crate::run::Run;

/// The executor name an actor's mount of a per-run copy asks for: the
/// control host itself, which no profile names.
// NOTICE: a profile's executor named this could not be reached by mounts:
// the relay answers this name before the run's executors.
pub const EXECUTOR: &str = "needy:control-host";

/// How long a merge driver session may run before it is cancelled, the store
/// left unchanged, and the run's copies kept as conflicts.
// NOTICE: 60 seconds covers booting a disposable merge session actor and
// running a row-level merge on a sizeable SQLite database while keeping the
// store's exclusive lock bounded when a driver hangs.
pub const MERGE_DRIVER_TIMEOUT: Duration = Duration::from_secs(60);

/// Marks a run whose end merged, or tried to merge, its per-run copies, so
/// a run that is still listed (finished, not yet stopped) counts as pending.
const ENDED: &str = "ended";

/// Serialized form of an `on-conflict` choice in `mapping.json`.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
enum StoredChoice {
  BuiltIn(String),
  Driver { driver: String },
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredUser {
  name: String,
  uid: u32,
  gid: u32,
}

/// What a merge driver session needs to boot the run's snapshot, persisted in
/// `mapping.json` so pending merges after a crash or daemon restart boot the
/// same image.
#[derive(serde::Serialize, serde::Deserialize)]
struct MergeSessionInfo {
  driver: String,
  base_container_image: Option<String>,
  snapshot_layers: Vec<String>,
  snapshot_image: Option<String>,
  image_path: String,
  user: StoredUser,
}

/// A sync mapping as the run was seeded with it, kept next to its copies so
/// a pending merge does not depend on the run's profile.
#[derive(serde::Serialize, serde::Deserialize)]
struct Record {
  name: String,
  store: PathBuf,
  filter: Vec<String>,
  /// Patterns and `store`, `run`, `lines`, or `{ driver: COMMAND }`, in order.
  on_conflict: Vec<(String, StoredChoice)>,
  /// Actor session metadata for running merge drivers; `None` for records
  /// created before merge drivers existed.
  #[serde(default)]
  session: Option<MergeSessionInfo>,
}

impl Record {
  fn policy(&self) -> Result<Policy> {
    let on_conflict = self
      .on_conflict
      .iter()
      .map(|(pattern, choice)| {
        let choice = match choice {
          StoredChoice::BuiltIn(name) => match name.as_str() {
            "store" => Choice::Store,
            "run" => Choice::Run,
            "lines" => Choice::Lines,
            other => bail!("unknown on-conflict choice {other:?}"),
          },
          StoredChoice::Driver { driver } => {
            if driver.trim().is_empty() {
              bail!("merge driver command must not be empty");
            }
            Choice::Driver(driver.clone())
          }
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

fn store_choice(choice: &OnConflict) -> StoredChoice {
  match choice {
    OnConflict::Store => StoredChoice::BuiltIn("store".into()),
    OnConflict::Run => StoredChoice::BuiltIn("run".into()),
    OnConflict::Lines => StoredChoice::BuiltIn("lines".into()),
    OnConflict::Driver(cmd) => StoredChoice::Driver {
      driver: cmd.clone(),
    },
  }
}

/// The account this daemon reads and writes stores as.
fn account() -> String {
  // SAFETY: `geteuid` cannot fail.
  std::env::var("USER").unwrap_or_else(|_| format!("uid {}", unsafe { libc::geteuid() }))
}

/// One effective sync mapping for a run, from either `profile.files` or a
/// feature's `customizations.needy.files`.
struct SyncMapping {
  name: String,
  store: PathBuf,
  target: String,
  filter: Vec<String>,
  on_conflict: Vec<(String, OnConflict)>,
}

/// `path` without trailing slashes, except the root.
fn trim_trailing_slashes(path: &str) -> &str {
  match path.trim_end_matches('/') {
    "" => path,
    trimmed => trimmed,
  }
}

/// The effective sync mappings of `run`: selected `mode: sync` entries from
/// `run.profile.files`, followed by feature-declared sync mappings from
/// `run.boot().sync_files` backed by `<daemon.data_dir>/files/<store>`.
///
/// Precedence: if `run.profile.files` explicitly defines a mapping with the
/// same name OR claims the same actor target path (via `copy-in`, `mount`, or
/// `sync`), the profile's mapping takes precedence and the feature's default
/// mapping is skipped. Feature-declared sync mappings are not filtered by
/// `profile.run_files` (which only accepts names declared in `profile.files`).
fn mappings(daemon: &Daemon, run: &Run) -> Vec<SyncMapping> {
  let mut out: Vec<SyncMapping> = run
    .profile
    .files
    .mappings
    .iter()
    .filter(|(name, _)| crate::files::selected(run, name))
    .filter_map(|(name, mapping)| match (mapping.mode, &mapping.from, &mapping.to) {
      (FileMode::Sync, Side::Host(store), Side::Actor(target)) => Some(SyncMapping {
        name: name.clone(),
        store: crate::files::host_path(run, store),
        target: target.clone(),
        filter: mapping.filter.clone(),
        on_conflict: mapping.on_conflict.clone(),
      }),
      _ => None,
    })
    .collect();

  let feature_files = run.boot().sync_files;
  if feature_files.is_empty() {
    return out;
  }

  let profile_claimed_paths: Vec<&str> = run
    .profile
    .files
    .mappings
    .values()
    .filter_map(|mapping| match (mapping.mode, &mapping.to) {
      (FileMode::CopyIn | FileMode::Mount | FileMode::Sync, Side::Actor(path)) => Some(trim_trailing_slashes(path)),
      _ => None,
    })
    .collect();

  for feature_file in feature_files {
    if run.profile.files.mappings.contains_key(&feature_file.name) {
      continue;
    }
    let norm = trim_trailing_slashes(&feature_file.path);
    if profile_claimed_paths.contains(&norm) {
      continue;
    }
    out.push(SyncMapping {
      name: feature_file.name,
      store: daemon.config.data_dir.join("files").join(&feature_file.store),
      target: feature_file.path,
      filter: feature_file.filter,
      on_conflict: feature_file.on_conflict,
    });
  }
  out
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

/// Holds the exclusive `flock(2)` on a store until dropped, so async merge
/// driver sessions can run while the store remains exclusively locked.
struct StoreLock(#[allow(dead_code)] std::fs::File);

/// Acquires the exclusive lock on `store` (a file per store under needy's data
/// directory locked with `flock(2)`).
// NOTICE: the lock is in this account's data directory rather than next
// to the store, so needy writes nothing into a store's parent; another
// account's daemon using the same store would not see it. The interim
// store is this account's (2026-11-17-images-and-run-data-spec.md).
fn acquire_lock(daemon_data: &Path, store: &Path) -> Result<StoreLock> {
  let locks = daemon_data.join("sync").join("locks");
  needy_api_server::create_private_dir(&locks).with_context(|| format!("create {}", locks.display()))?;
  let key = hex(&Sha256::digest(store.as_os_str().as_encoded_bytes()));
  let path = locks.join(format!("{key}.lock"));
  let file =
    std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path).with_context(|| format!("open {}", path.display()))?;
  file.lock().with_context(|| format!("lock the store {}", store.display()))?;
  Ok(StoreLock(file))
}

/// Runs `f` holding the exclusive lock on `store`.
fn locked<T>(daemon_data: &Path, store: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
  let _lock = acquire_lock(daemon_data, store)?;
  f()
}

/// Acquires the exclusive lock on `store` without blocking the async runtime.
async fn lock_store(daemon_data: PathBuf, store: PathBuf) -> Result<StoreLock> {
  tokio::task::spawn_blocking(move || acquire_lock(&daemon_data, &store)).await?
}

fn hex(bytes: &[u8]) -> String {
  bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Checks the store of every sync mapping `run` uses, before the run
/// acquires anything: a store under needy's data directory is created when
/// missing; any other is the user's and must exist on the control host.
pub fn check(daemon: &Daemon, run: &Run) -> Result<()> {
  for mapping in mappings(daemon, run) {
    let name = &mapping.name;
    let store = &mapping.store;
    if store.starts_with(&daemon.config.data_dir) {
      needy_api_server::create_private_dir(store).with_context(|| format!("sync {name}: create the store {}", store.display()))?;
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
  for mapping in mappings(daemon, run) {
    let name = &mapping.name;
    let store = &mapping.store;
    drain_store(daemon, store).await;
    let dir = run_dir(daemon, &run.id).join(name);
    needy_api_server::create_private_dir(&dir).with_context(|| format!("create {}", dir.display()))?;
    let boot = run.boot();
    let session = MergeSessionInfo {
      driver: run.profile.actor.driver.as_str().to_string(),
      base_container_image: Some(run.profile.actor.container_image()),
      snapshot_layers: boot.snapshot.as_ref().map(|s| s.layers.iter().map(ToString::to_string).collect()).unwrap_or_default(),
      snapshot_image: boot.snapshot_image.clone(),
      image_path: boot.path.clone(),
      user: StoredUser {
        name: run.profile.user.name.clone(),
        uid: run.profile.user.uid,
        gid: run.profile.user.gid,
      },
    };
    let record = Record {
      name: name.clone(),
      store: store.clone(),
      filter: mapping.filter.clone(),
      on_conflict: mapping.on_conflict.iter().map(|(pattern, choice)| (pattern.clone(), store_choice(choice))).collect(),
      session: Some(session),
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
  mappings(daemon, run)
    .into_iter()
    .filter_map(|mapping| {
      let copy = copy_dir(daemon, &run.id, &mapping.name);
      copy.is_dir().then(|| HostMount {
        target: mapping.target,
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
    let merged = merge_mapping(daemon, id, &record, &mapping_dir, &kept, trace).await;
    let event = match merged {
      Ok(files_merge) => {
        remove(&mapping_dir);
        Event::FilesMerge(files_merge)
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

/// Outcome of executing one merge driver inside an actor session.
enum DriverOutcome {
  /// The driver exited 0 and its output was applied to the store; carries the
  /// number of entries resolved.
  Merged(u64),
  /// The driver exited non-zero, timed out, or produced invalid output; the
  /// store was left unchanged and the run's copies must be kept as conflicts.
  Failed(String),
}

/// Merges one sync mapping under the store's exclusive lock: runs needy's
/// built-in three-way merge first, then hands any `driver_claims` to their
/// merge driver commands inside disposable actor sessions.
async fn merge_mapping(daemon: &Daemon, id: &str, record: &Record, mapping_dir: &Path, kept: &Path, trace: &Dispatch) -> Result<FilesMerge> {
  let policy = record.policy()?;
  let _lock = lock_store(daemon.config.data_dir.clone(), record.store.clone()).await?;

  let outcome = {
    let (mapping_dir, kept, store) = (mapping_dir.to_path_buf(), kept.to_path_buf(), record.store.clone());
    tokio::task::spawn_blocking(move || {
      let trees = Trees {
        seed: &mapping_dir.join("seed"),
        run: &mapping_dir.join("copy"),
        store: &store,
      };
      merge::merge(trees, &kept, &policy)
    })
    .await??
  };

  let count = |pick: fn(&merge::Action) -> bool| outcome.changes.iter().filter(|c| pick(&c.action)).count() as u64;
  let taken = count(|a| matches!(a, merge::Action::Taken | merge::Action::Removed));
  let mut resolved = count(|a| matches!(a, merge::Action::Resolved(_)));
  let mut conflicts: Vec<String> = outcome.conflicts().map(str::to_string).collect();

  // TODO(merge-drivers): at a normal end, running the merge driver inside the
  // run's still-running actor before shutdown (instead of booting a short
  // merge session) is deferred cleanly; the merge session path handles normal
  // stop, `--now`, crash, and pending-merge recovery uniformly without a
  // guest Protobuf change. See 2026-11-17-images-and-run-data-spec.md.
  for claim in &outcome.driver_claims {
    match run_merge_driver(daemon, record, mapping_dir, claim, trace).await? {
      DriverOutcome::Merged(n) => {
        resolved += n;
      }
      DriverOutcome::Failed(reason) => {
        tracing::warn!(
          "run {id}: sync {}: merge driver `{}` failed ({reason}); keeping store unchanged and saving run's copies",
          record.name,
          claim.driver
        );
        let (run_copy, paths, kept_dir) = (mapping_dir.join("copy"), claim.paths.clone(), kept.to_path_buf());
        tokio::task::spawn_blocking(move || merge::keep_driver_conflicts(&run_copy, &paths, &kept_dir)).await??;
        conflicts.extend(claim.paths.iter().cloned());
      }
    }
  }

  conflicts.sort();
  conflicts.dedup();
  Ok(FilesMerge {
    name: record.name.clone(),
    store: record.store(),
    taken,
    resolved,
    kept: (!conflicts.is_empty()).then(|| kept.display().to_string()),
    conflicts,
  })
}

/// Runs one merge driver command inside a disposable actor session with
/// routing off, network off, and a bounded time limit (`merge_driver_timeout`).
async fn run_merge_driver(
  daemon: &Daemon,
  record: &Record,
  mapping_dir: &Path,
  claim: &DriverClaim,
  trace: &Dispatch,
) -> Result<DriverOutcome> {
  // Stage the three trees (`seed/`, `run/`, `store/`) and pack them into
  // uncompressed tars for `copy_in`.
  let (seed_tar, run_tar, store_tar) = {
    let mapping_dir = mapping_dir.to_path_buf();
    let store = record.store.clone();
    let paths = claim.paths.clone();
    tokio::task::spawn_blocking(move || -> Result<(Vec<u8>, Vec<u8>, Vec<u8>)> {
      let staging = mapping_dir.join(format!(".driver-stage-{}", uuid::Uuid::new_v4()));
      needy_api_server::create_private_dir(&staging).context("create merge driver staging dir")?;
      let result = (|| -> Result<(Vec<u8>, Vec<u8>, Vec<u8>)> {
        let trees = Trees {
          seed: &mapping_dir.join("seed"),
          run: &mapping_dir.join("copy"),
          store: &store,
        };
        merge::stage_driver_trees(trees, &paths, &staging)?;
        let seed_tar = needy_common::archive::pack(&staging.join("seed"), &[])?;
        let run_tar = needy_common::archive::pack(&staging.join("run"), &[])?;
        let store_tar = needy_common::archive::pack(&staging.join("store"), &[])?;
        Ok((seed_tar, run_tar, store_tar))
      })();
      remove(&staging);
      result
    })
    .await??
  };

  let (target_driver, base_container_image, snapshot_layers, snapshot_image, image_path, user) = match &record.session {
    Some(info) => {
      let driver = ActorDriver::ALL.into_iter().find(|d| d.as_str() == info.driver).unwrap_or_else(needy_core_actor::image::vm::host_driver);
      (
        driver,
        info.base_container_image.clone(),
        info.snapshot_layers.clone(),
        info.snapshot_image.clone(),
        info.image_path.clone(),
        User {
          name: info.user.name.clone(),
          uid: info.user.uid,
          gid: info.user.gid,
        },
      )
    }
    None => (needy_core_actor::image::vm::host_driver(), None, Vec::new(), None, String::new(), User::default()),
  };

  // Apple's `container` CLI cannot run build/merge sessions directly; use the
  // host's session builder driver (`macos-vz`) when needed.
  let session_driver = if target_driver == ActorDriver::MacosAppleContainer {
    crate::image::select_builder_driver(daemon, target_driver).await
  } else {
    target_driver
  };

  let (snapshot, layer_disks, resolved_snapshot_image) = if snapshot_layers.is_empty() {
    (None, Vec::new(), snapshot_image)
  } else {
    let layers: Vec<Digest> = snapshot_layers
      .iter()
      .map(|s| s.parse::<Digest>().map_err(|e| anyhow!("invalid snapshot layer digest {s:?}: {e}")))
      .collect::<Result<_>>()?;
    if session_driver == ActorDriver::LinuxContainer {
      let store = daemon.images.store.clone();
      let snapshot = tokio::task::spawn_blocking(move || store.snapshot(&layers)).await.context("snapshotting layers panicked")??;
      let tag = match snapshot_image {
        Some(tag) => tag,
        None => {
          let base = base_container_image.as_deref().unwrap_or(needy_config::profile::CONTAINER_IMAGE);
          crate::image::materialize_for_linux_container(daemon, &snapshot, base).await?
        }
      };
      (Some(snapshot), Vec::new(), Some(tag))
    } else {
      let store = daemon.images.store.clone();
      let (snapshot, disks) = tokio::task::spawn_blocking(move || -> Result<_> {
        let snapshot = store.snapshot(&layers)?;
        let disks = store.materialize(&snapshot)?;
        Ok((snapshot, disks))
      })
      .await
      .context("materializing merge session layers panicked")??;
      (Some(snapshot), disks, None)
    }
  };

  // Three-tree driver contract: `<driver> <seed_dir> <run_dir> <store_dir> <out_dir>`.
  let spec = SessionSpec {
    driver: session_driver,
    base_container_image,
    snapshot,
    layer_disks,
    snapshot_image: resolved_snapshot_image,
    image_path,
    user,
    command: vec![
      "sh".into(),
      "-c".into(),
      format!(r#"mkdir -p "$1" "$2" "$3" "$4" && exec {} "$1" "$2" "$3" "$4""#, claim.driver),
      "sh".into(),
      "/tmp/needy-merge/seed".into(),
      "/tmp/needy-merge/run".into(),
      "/tmp/needy-merge/store".into(),
      "/tmp/needy-merge/out".into(),
    ],
    cwd: "/".into(),
    env: BTreeMap::new(),
    copy_in: vec![
      ("seed".into(), "/tmp/needy-merge/seed".into(), seed_tar),
      ("run".into(), "/tmp/needy-merge/run".into(), run_tar),
      ("store".into(), "/tmp/needy-merge/store".into(), store_tar),
    ],
    mirror_out: vec![("out".into(), "/tmp/needy-merge/out".into())],
    route_names: Vec::new(),
    session: GuestSession {
      upper: UpperLayer::Disposable,
      routing: false,
    },
    tty: None,
    wait_for_attach: false,
    // Network completely off: empty allowlist denies every outbound connection.
    network: needy_core::network::Policy {
      allow: Vec::new(),
      services: BTreeMap::new(),
    },
    trace: trace.clone(),
  };

  let session_cancel = daemon.cancel.child_token();
  let timeout = daemon.config.merge_driver_timeout;
  let timed_out = Arc::new(AtomicBool::new(false));
  let timed_out_bg = timed_out.clone();
  let cancel_bg = session_cancel.clone();
  let timer = tokio::spawn(async move {
    tokio::time::sleep(timeout).await;
    timed_out_bg.store(true, Ordering::SeqCst);
    cancel_bg.cancel();
  });

  let session_result = crate::image::run_session(daemon, spec, session_cancel).await;
  timer.abort();

  if timed_out.load(Ordering::SeqCst) {
    return Ok(DriverOutcome::Failed(format!("timed out after {:.1}s", timeout.as_secs_f64())));
  }
  if daemon.cancel.is_cancelled() {
    bail!("daemon shutting down while merge driver `{}` was running", claim.driver);
  }

  let outcome = match session_result {
    Ok(outcome) => outcome,
    Err(e) => return Ok(DriverOutcome::Failed(format!("merge session failed: {e:#}"))),
  };

  match outcome.exit {
    Some(Exit {
      code: Some(0),
      signal,
    }) if signal.is_empty() => {}
    Some(Exit {
      code: Some(code),
      signal,
    }) if signal.is_empty() => {
      return Ok(DriverOutcome::Failed(format!("exited with code {code}")));
    }
    Some(Exit { signal, .. }) if !signal.is_empty() => {
      return Ok(DriverOutcome::Failed(format!("killed by {signal}")));
    }
    _ => {
      return Ok(DriverOutcome::Failed("exited before reporting its status".into()));
    }
  }

  // Unpack `out` mirror-out archive and apply atomically to the store.
  let out_archive = outcome.mirrored_out.get("out").cloned().unwrap_or_default();
  let store = record.store.clone();
  let claimed = claim.paths.clone();
  let mapping_dir = mapping_dir.to_path_buf();
  let placed = tokio::task::spawn_blocking(move || -> Result<Vec<String>> {
    let out_dir = mapping_dir.join(format!(".driver-out-{}", uuid::Uuid::new_v4()));
    needy_api_server::create_private_dir(&out_dir).context("create merge driver output dir")?;
    let result = (|| -> Result<Vec<String>> {
      if !out_archive.is_empty() {
        needy_common::archive::unpack(&out_archive, &out_dir, &[])?;
      }
      merge::apply_driver_output(&out_dir, &store, &claimed)
    })();
    remove(&out_dir);
    result
  })
  .await??;

  Ok(DriverOutcome::Merged(placed.len().max(1) as u64))
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
