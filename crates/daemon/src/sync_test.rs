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

/// A stand-in Firecracker that logs every started VM directory (`state/runs/`
/// or `state/sessions/`) to `started_log` and waits until `<dir>/done` appears
/// or its parent exits.
fn merge_session_driver_script(started_log: &Path) -> String {
  format!(
    r#"dir=$(dirname "$3")
touch "$dir/v.sock"
echo "$dir" >> "{}"
while [ ! -e "$dir/done" ] && kill -0 $PPID 2>/dev/null; do sleep 0.02; done"#,
    started_log.display()
  )
}

#[derive(Clone)]
enum MergeResponderMode {
  /// Fetches `seed`, `run`, `store`, verifies them, pushes `merged_content` as
  /// `opencode.db` in `out`, and exits 0.
  Succeed { merged_content: String },
  /// Exits with the given non-zero code without pushing `out`.
  Exit(i32),
  /// Never finishes (for timeout tests).
  Hang,
}

type ObservedConfigs = std::sync::Arc<std::sync::Mutex<Vec<needy_api_client::proto::v1::GetConfigResponse>>>;
type ObservedTrees = std::sync::Arc<std::sync::Mutex<Vec<(String, String, String)>>>;

/// Drives merge sessions started under `state/sessions/` and returns the
/// observed `GetConfigResponse`s and fetched `(seed, run, store)` file maps.
fn spawn_merge_responder(
  started_log: PathBuf,
  mode: MergeResponderMode,
  stop: tokio_util::sync::CancellationToken,
) -> (ObservedConfigs, ObservedTrees, tokio::task::JoinHandle<()>) {
  use needy_api_client::proto::v1::{FetchFilesRequest, GetConfigRequest, PushFilesRequest, ReportExitRequest};
  let configs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
  let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
  let configs_bg = configs.clone();
  let observed_bg = observed.clone();

  let handle = tokio::spawn(async move {
    let mut seen = std::collections::HashSet::new();
    while !stop.is_cancelled() {
      if let Ok(text) = std::fs::read_to_string(&started_log) {
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
          let dir = PathBuf::from(line);
          if !seen.insert(dir.clone()) {
            continue;
          }
          if !dir.parent().and_then(|p| p.file_name()).is_some_and(|n| n == "sessions") {
            continue;
          }
          let configs_task = configs_bg.clone();
          let observed_task = observed_bg.clone();
          let mode_task = mode.clone();
          tokio::spawn(async move {
            let sock = dir.join("v.sock_7000");
            let endpoint = format!("unix:{}", sock.display());
            let client = needy_api_client::Client::connect(&endpoint).await.unwrap();
            let config = client.guest().get_config(GetConfigRequest {}).await.unwrap().into_inner();
            configs_task.lock().unwrap().push(config.clone());

            // Fetch seed, run, store archives and record opencode.db from each.
            let mut fetched = std::collections::HashMap::new();
            for copy in &config.copy_in {
              let mut stream = client
                .guest()
                .fetch_files(FetchFilesRequest {
                  name: copy.name.clone(),
                })
                .await
                .unwrap()
                .into_inner();
              let mut archive = Vec::new();
              while let Some(chunk) = stream.message().await.unwrap() {
                archive.extend(chunk.data);
              }
              let tmp = tempfile::tempdir().unwrap();
              needy_common::archive::unpack(&archive, tmp.path(), &[]).unwrap();
              let db = std::fs::read_to_string(tmp.path().join("opencode.db")).unwrap_or_default();
              fetched.insert(copy.name.clone(), db);
            }
            observed_task.lock().unwrap().push((
              fetched.get("base").cloned().unwrap_or_default(),
              fetched.get("run").cloned().unwrap_or_default(),
              fetched.get("current").cloned().unwrap_or_default(),
            ));

            match mode_task {
              MergeResponderMode::Hang => {
                // Leave the VM running until the daemon's timeout cancels it.
                return;
              }
              MergeResponderMode::Exit(code) => {
                let _ = client
                  .guest()
                  .report_exit(ReportExitRequest {
                    code: Some(code),
                    signal: String::new(),
                  })
                  .await;
              }
              MergeResponderMode::Succeed { merged_content } => {
                let out_tmp = tempfile::tempdir().unwrap();
                std::fs::write(out_tmp.path().join("opencode.db"), merged_content).unwrap();
                let archive = needy_common::archive::pack(out_tmp.path(), &[]).unwrap();
                client
                  .guest()
                  .push_files(tokio_stream::iter(vec![PushFilesRequest {
                    name: "merged".into(),
                    data: archive,
                  }]))
                  .await
                  .unwrap();
                client
                  .guest()
                  .report_exit(ReportExitRequest {
                    code: Some(0),
                    signal: String::new(),
                  })
                  .await
                  .unwrap();
              }
            }
            let _ = std::fs::write(dir.join("done"), "");
          });
        }
      }
      tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
  });
  (configs, observed, handle)
}

#[tokio::test]
async fn a_merge_driver_receives_three_trees_in_an_actor_session_and_updates_the_store() {
  use needy_api_client::proto::v1::UpperLayer as ProtoUpperLayer;

  let started = testing::daemon().await;
  let (dir, data) = (started.dir.path(), started.dir.path().join("data"));
  let started_log = dir.join("started.log");
  testing::fake_image(&dir.join("image"), &merge_session_driver_script(&started_log));

  let store = dir.join("store");
  write(&store, "opencode.db", "seed-db");
  write(&store, "opencode.db-wal", "seed-wal");
  write(&store, "notes.txt", "seed-note");

  let cancel = tokio_util::sync::CancellationToken::new();
  let (configs, observed, responder) = spawn_merge_responder(
    started_log,
    MergeResponderMode::Succeed {
      merged_content: "merged-db".into(),
    },
    cancel.clone(),
  );

  let id = create(&started.daemon, dir, &profile(&store, "\"opencode.db*\": { driver: needy-merge-sqlite }")).await;
  write(&copy(&data, &id), "opencode.db", "run-db");
  write(&copy(&data, &id), "opencode.db-wal", "run-wal");
  write(&copy(&data, &id), "notes.txt", "run-note");
  write(&store, "opencode.db", "store-db");
  write(&store, "opencode.db-wal", "store-wal");

  stop(&started.daemon, &id).await;
  cancel.cancel();
  let _ = responder.await;

  // Store got merged db, stale -wal was removed, and non-driver file merged normally.
  assert_eq!(read(&store, "opencode.db").as_deref(), Some("merged-db"));
  assert!(!store.join("opencode.db-wal").exists(), "stale -wal side file must be cleaned up");
  assert_eq!(read(&store, "notes.txt").as_deref(), Some("run-note"));

  // Driver session ran with routing off, disposable upper layer, and received all three trees.
  let cfgs = configs.lock().unwrap().clone();
  assert_eq!(cfgs.len(), 1);
  let sess = cfgs[0].session.unwrap();
  assert!(!sess.routing);
  assert_eq!(sess.upper(), ProtoUpperLayer::Disposable);
  assert!(cfgs[0].command.iter().any(|arg| arg.contains("needy-merge-sqlite")));
  assert_eq!(observed.lock().unwrap().as_slice(), &[("seed-db".into(), "run-db".into(), "store-db".into())]);

  let json = summary(dir, &id).await;
  assert_eq!(json["synced"][0]["merge"], "merged");
  assert_eq!(json["synced"][0]["conflicts"], serde_json::json!([]));
  assert_eq!(json["findings"], serde_json::json!([]));
}

#[tokio::test]
async fn a_failing_merge_driver_leaves_the_store_unchanged_and_reports_a_conflict() {
  let started = testing::daemon().await;
  let (dir, data) = (started.dir.path(), started.dir.path().join("data"));
  let started_log = dir.join("started.log");
  testing::fake_image(&dir.join("image"), &merge_session_driver_script(&started_log));

  let store = dir.join("store");
  write(&store, "opencode.db", "seed-db");

  let cancel = tokio_util::sync::CancellationToken::new();
  let (_configs, _observed, responder) = spawn_merge_responder(started_log, MergeResponderMode::Exit(1), cancel.clone());

  let id = create(&started.daemon, dir, &profile(&store, "\"opencode.db*\": { driver: needy-merge-sqlite }")).await;
  write(&copy(&data, &id), "opencode.db", "run-db");
  write(&copy(&data, &id), "opencode.db-wal", "run-wal");
  write(&store, "opencode.db", "store-db");

  stop(&started.daemon, &id).await;
  cancel.cancel();
  let _ = responder.await;

  assert_eq!(read(&store, "opencode.db").as_deref(), Some("store-db"), "store must remain unchanged on driver failure");
  let kept = data.join("conflicts").join(&id).join("pi");
  assert_eq!(read(&kept, "opencode.db").as_deref(), Some("run-db"));
  assert_eq!(read(&kept, "opencode.db-wal").as_deref(), Some("run-wal"));

  let json = summary(dir, &id).await;
  assert_eq!(json["synced"][0]["merge"], "merged");
  assert_eq!(json["synced"][0]["conflicts"], serde_json::json!(["opencode.db", "opencode.db-wal"]));
  assert_eq!(json["synced"][0]["kept"], kept.display().to_string());
  assert_eq!(json["findings"][0]["kind"], "sync_conflict");
}

#[tokio::test]
async fn a_timed_out_merge_driver_leaves_the_store_unchanged_and_reports_a_conflict() {
  let started = testing::daemon_with(|config| {
    config.merge_driver_timeout = std::time::Duration::from_millis(250);
  })
  .await;
  let (dir, data) = (started.dir.path(), started.dir.path().join("data"));
  let started_log = dir.join("started.log");
  testing::fake_image(&dir.join("image"), &merge_session_driver_script(&started_log));

  let store = dir.join("store");
  write(&store, "opencode.db", "seed-db");

  let cancel = tokio_util::sync::CancellationToken::new();
  let (_configs, _observed, responder) = spawn_merge_responder(started_log, MergeResponderMode::Hang, cancel.clone());

  let id = create(&started.daemon, dir, &profile(&store, "\"opencode.db*\": { driver: needy-merge-sqlite }")).await;
  write(&copy(&data, &id), "opencode.db", "run-db");
  write(&store, "opencode.db", "store-db");

  tokio::time::timeout(std::time::Duration::from_secs(10), stop(&started.daemon, &id))
    .await
    .expect("timed-out merge driver must not hang stop");
  cancel.cancel();
  let _ = responder.await;

  assert_eq!(read(&store, "opencode.db").as_deref(), Some("store-db"));
  let kept = data.join("conflicts").join(&id).join("pi");
  assert_eq!(read(&kept, "opencode.db").as_deref(), Some("run-db"));

  let json = summary(dir, &id).await;
  assert_eq!(json["synced"][0]["conflicts"], serde_json::json!(["opencode.db"]));
  assert_eq!(json["findings"][0]["kind"], "sync_conflict");
}
