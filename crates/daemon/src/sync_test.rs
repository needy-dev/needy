use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use needy_api_server::control::{ControlError, Guest, Runs, StopMode};
use needy_core::event::{Event, Recorded};
use needy_core::report::{self, Format, Level};
use needy_core::types::Principal;

use super::EXECUTOR;
use crate::testing::{self, new_run};

/// A run whose actor syncs `~/.pi/agent` with the store `store`.
fn profile(store: &Path, on_conflict: &str) -> String {
  format!(
    r#"
user: {{ name: agent, uid: 1000, gid: 1000 }}
actor: {{ driver: linux-firecracker }}
files:
  pi:
    mode: sync
    from: "host:{}"
    to: "actor:~/.pi/agent"
    filter: ["- cache/"]
    on-conflict: {{ {on_conflict} }}
"#,
    store.display()
  )
}

fn write(dir: &Path, path: &str, text: &str) {
  let path = dir.join(path);
  std::fs::create_dir_all(path.parent().unwrap()).unwrap();
  std::fs::write(path, text).unwrap();
}

fn read(dir: &Path, path: &str) -> Option<String> {
  std::fs::read_to_string(dir.join(path)).ok()
}

/// The per-run copy of run `id`'s mapping `pi`, which its actor writes.
fn copy(data: &Path, id: &str) -> PathBuf {
  data.join("sync/runs").join(id).join("pi/copy")
}

async fn create(daemon: &crate::Daemon, dir: &Path, yaml: &str) -> String {
  Runs::create(daemon, new_run(yaml, dir)).await.unwrap().id
}

async fn stop(daemon: &crate::Daemon, id: &str) {
  Runs::stop(daemon, id, Principal::Operator, StopMode::Now).await.unwrap();
}

/// Run `id`'s trace, decoded, as `needy report` reads it.
async fn events(dir: &Path, id: &str) -> Vec<Recorded> {
  let trace = needy_trace::sink::sqlite::load(&dir.join("state/trace.db"), id).await.unwrap();
  trace
    .events
    .iter()
    .map(|e| Recorded {
      time_ms: e.time_ms,
      event: Event::decode(&e.kind, e.version, e.payload.get()).unwrap(),
    })
    .collect()
}

async fn summary(dir: &Path, id: &str) -> serde_json::Value {
  serde_json::from_str(&report::render(id, &events(dir, id).await, Level::Summary, Format::Json)).unwrap()
}

#[tokio::test]
async fn a_run_mounts_a_per_run_copy_of_its_store_from_the_control_host() {
  let started = testing::daemon().await;
  let (dir, data) = (started.dir.path(), started.dir.path().join("data"));
  let store = dir.join("store");
  write(&store, "auth.json", "token");
  write(&store, "cache/blob", "x");

  let id = create(&started.daemon, dir, &profile(&store, "")).await;
  let config = Guest::config(&started.daemon, &id).await.unwrap();

  let copy = copy(&data, &id);
  assert_eq!(read(&copy, "auth.json").as_deref(), Some("token"));
  assert!(!copy.join("cache").exists(), "the filter leaves caches out of the per-run copy");
  let mount = config.mounts.iter().find(|m| m.target == "/home/agent/.pi/agent").expect("a mount of the per-run copy");
  assert_eq!((mount.executor.as_str(), mount.source.as_str(), mount.writable), (EXECUTOR, copy.to_str().unwrap(), true));
  let json = summary(dir, &id).await;
  assert_eq!(json["synced"][0]["store"], store.display().to_string());
  assert_eq!(json["synced"][0]["merge"], "unmerged");
  stop(&started.daemon, &id).await;
}

#[tokio::test]
async fn a_stopped_runs_changes_are_merged_into_the_store_and_its_copies_removed() {
  let started = testing::daemon().await;
  let (dir, data) = (started.dir.path(), started.dir.path().join("data"));
  let store = dir.join("store");
  write(&store, "auth.json", "old token");
  write(&store, "models.json", "old");
  let id = create(&started.daemon, dir, &profile(&store, "")).await;
  // What the actor writes in its per-run copy, and what another run merged
  // into the store meanwhile.
  write(&copy(&data, &id), "auth.json", "refreshed token");
  write(&copy(&data, &id), "sessions/s1.jsonl", "hello");
  write(&store, "models.json", "newer");

  stop(&started.daemon, &id).await;

  assert_eq!(read(&store, "auth.json").as_deref(), Some("refreshed token"));
  assert_eq!(read(&store, "sessions/s1.jsonl").as_deref(), Some("hello"));
  assert_eq!(read(&store, "models.json").as_deref(), Some("newer"), "a file the run left alone keeps the store's newer copy");
  assert!(!data.join("sync/runs").join(&id).exists(), "the seed and the per-run copy are removed after the merge");
  let json = summary(dir, &id).await;
  assert_eq!(json["synced"][0]["merge"], "merged");
  assert_eq!(json["findings"], serde_json::json!([]));
}

#[tokio::test]
async fn a_file_both_changed_keeps_the_stores_and_reports_the_runs_kept_aside() {
  let started = testing::daemon().await;
  let (dir, data) = (started.dir.path(), started.dir.path().join("data"));
  let store = dir.join("store");
  write(&store, "installation_id", "seed");
  write(&store, "history.jsonl", "a\n");
  let id = create(&started.daemon, dir, &profile(&store, "history.jsonl: lines")).await;
  write(&copy(&data, &id), "installation_id", "run");
  write(&store, "installation_id", "store");
  write(&copy(&data, &id), "history.jsonl", "a\nfrom run\n");
  write(&store, "history.jsonl", "a\nfrom another run\n");

  stop(&started.daemon, &id).await;

  assert_eq!(read(&store, "installation_id").as_deref(), Some("store"));
  assert_eq!(read(&store, "history.jsonl").as_deref(), Some("a\nfrom another run\nfrom run\n"));
  let kept = data.join("conflicts").join(&id).join("pi");
  assert_eq!(read(&kept, "installation_id").as_deref(), Some("run"));
  let json = summary(dir, &id).await;
  assert_eq!(json["synced"][0]["conflicts"], serde_json::json!(["installation_id"]));
  assert_eq!(json["synced"][0]["kept"], kept.display().to_string());
  assert_eq!(json["findings"][0]["kind"], "sync_conflict");
}

#[tokio::test]
async fn two_runs_of_one_store_both_keep_their_sessions() {
  let started = testing::daemon().await;
  let (dir, data) = (started.dir.path(), started.dir.path().join("data"));
  let store = dir.join("store");
  write(&store, "auth.json", "token");
  let yaml = profile(&store, "");
  let first = create(&started.daemon, dir, &yaml).await;
  let second = create(&started.daemon, dir, &yaml).await;
  write(&copy(&data, &first), "sessions/one.jsonl", "1");
  write(&copy(&data, &first), "auth.json", "refreshed by one");
  write(&copy(&data, &second), "sessions/two.jsonl", "2");

  // Stopped at once, so their merges take the store's lock in turn.
  tokio::join!(stop(&started.daemon, &first), stop(&started.daemon, &second));

  assert_eq!(read(&store, "sessions/one.jsonl").as_deref(), Some("1"));
  assert_eq!(read(&store, "sessions/two.jsonl").as_deref(), Some("2"));
  assert_eq!(read(&store, "auth.json").as_deref(), Some("refreshed by one"), "the run that left auth.json alone did not overwrite it");
}

#[tokio::test]
async fn a_merge_that_cannot_run_stays_pending_and_runs_before_the_store_is_seeded_again() {
  let started = testing::daemon().await;
  let (dir, data) = (started.dir.path(), started.dir.path().join("data"));
  let store = dir.join("store");
  write(&store, "auth.json", "old");
  let yaml = profile(&store, "");
  let first = create(&started.daemon, dir, &yaml).await;
  write(&copy(&data, &first), "auth.json", "refreshed");
  // A store the daemon cannot write, as one on an unreachable disk.
  std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o500)).unwrap();

  stop(&started.daemon, &first).await;

  std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o700)).unwrap();
  assert_eq!(read(&store, "auth.json").as_deref(), Some("old"));
  assert_eq!(summary(dir, &first).await["synced"][0]["merge"], "pending");
  assert!(copy(&data, &first).join("auth.json").exists(), "a pending merge keeps the run's copy");

  let next = create(&started.daemon, dir, &yaml).await;

  assert_eq!(read(&copy(&data, &next), "auth.json").as_deref(), Some("refreshed"), "the next run is seeded after the pending merge");
  assert_eq!(summary(dir, &first).await["synced"][0]["merge"], "merged");
  assert!(!data.join("sync/runs").join(&first).exists());
  stop(&started.daemon, &next).await;
}

#[tokio::test]
async fn pending_merges_run_when_the_daemon_starts() {
  let started = testing::daemon().await;
  let (dir, data) = (started.dir.path().to_path_buf(), started.dir.path().join("data"));
  let store = dir.join("store");
  write(&store, "auth.json", "old");
  let id = create(&started.daemon, &dir, &profile(&store, "")).await;
  write(&copy(&data, &id), "auth.json", "refreshed");
  std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o500)).unwrap();
  stop(&started.daemon, &id).await;
  std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o700)).unwrap();

  let restarted = testing::restart(started).await;

  assert_eq!(read(&store, "auth.json").as_deref(), Some("refreshed"));
  assert_eq!(summary(&dir, &id).await["synced"][0]["merge"], "merged");
  drop(restarted);
}

#[tokio::test]
async fn a_store_under_needys_data_directory_is_created_and_any_other_must_exist() {
  let started = testing::daemon().await;
  let dir = started.dir.path();
  let ours = dir.join("data/files/pi");
  let theirs = dir.join("missing/store");

  let id = create(&started.daemon, dir, &profile(&ours, "")).await;
  let refused = Runs::create(&started.daemon, new_run(&profile(&theirs, ""), dir)).await;

  assert!(ours.is_dir());
  let Err(ControlError::Failed(message)) = refused else {
    panic!("a missing store outside needy's data directory must fail the create, not {refused:?}");
  };
  assert!(message.contains(&format!("the store {} does not exist on the control host", theirs.display())), "{message}");
  assert!(!theirs.exists());
  stop(&started.daemon, &id).await;
}

#[tokio::test]
async fn the_control_host_serves_an_actor_only_the_mounts_of_its_per_run_copies() {
  use needy_api_server::control::{ExecOpen, ExecSpec, Processes};
  let started = testing::daemon().await;
  let dir = started.dir.path();
  let store = dir.join("store");
  std::fs::create_dir_all(&store).unwrap();
  let id = create(&started.daemon, dir, &profile(&store, "")).await;
  let spec = |argv: Vec<String>, cwd: &str| ExecSpec {
    argv,
    cwd: cwd.into(),
    env: Default::default(),
    clear_env: false,
    tty: None,
    detach: false,
    run: id.clone(),
    executor: EXECUTOR.into(),
    runner: String::new(),
    no_stdin: false,
    origin: needy_core::types::Origin::ActorMount,
    search_path_if_missing: false,
    by: None,
  };
  let sftp = started.daemon.local.handle.sftp.clone();

  let other_dir = started.daemon.exec(ExecOpen::Start(spec(sftp, "/")), Box::pin(tokio_stream::pending())).await;
  let other_program = started
    .daemon
    .exec(ExecOpen::Start(spec(vec!["sh".into()], &copy(&dir.join("data"), &id).display().to_string())), Box::pin(tokio_stream::pending()))
    .await;

  assert!(matches!(other_dir, Err(ControlError::Denied { .. })), "{:?}", other_dir.err());
  assert!(matches!(other_program, Err(ControlError::Denied { .. })), "{:?}", other_program.err());
  stop(&started.daemon, &id).await;
}
