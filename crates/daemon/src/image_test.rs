use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use needy_api_client::proto::v1::{GetConfigRequest, GetConfigResponse, ReportExitRequest, UpperLayer as ProtoUpperLayer};
use needy_api_server::control::{ControlError, Guest, GuestSession, Runs, UpperLayer};
use needy_core::event::{Event, Recorded};
use needy_core::report::{self, Format, Level};
use tokio::sync::Notify;

use crate::testing::{self, fake_image, new_run};

const IMAGE_PROFILE: &str = r#"
actor:
  driver: linux-firecracker
  env:
    SHARED: from-profile
    PROFILE_ONLY: yes
  image:
    root: needy-actor:latest
    env:
      PATH: /opt/tools/bin:${PATH}
      SHARED: from-image
      IMAGE_ONLY: "1"
    layers:
      - run: echo step-one
      - run: echo step-two
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  here: { driver: native }
commands:
  - { match: "**", to: here }
"#;

/// A stand-in Firecracker that logs its directory to `<started_log>` and
/// waits until `<dir>/done` is created (or its parent exits, for the final
/// run VM).
fn session_driver_script(started_log: &Path) -> String {
  format!(
    r#"dir=$(dirname "$3")
touch "$dir/v.sock"
echo "$dir" >> "{}"
while [ ! -e "$dir/done" ] && kill -0 $PPID 2>/dev/null; do sleep 0.02; done"#,
    started_log.display()
  )
}

/// Packs a single file `name` with `content` into an uncompressed tar archive,
/// as the guest's upper-layer archiver produces for `Store::put`.
fn tar_with_file(name: &str, content: &str) -> Vec<u8> {
  let dir = tempfile::tempdir().unwrap();
  std::fs::write(dir.path().join(name), content).unwrap();
  needy_common::archive::pack(dir.path(), &[]).unwrap()
}

/// What a simulated build-session guest returns to the daemon.
#[derive(Default)]
struct BuildBehavior {
  /// Optional gate: wait on this notify before completing the session (used
  /// to hold the first build in flight for single-flight tests).
  gate: Option<Arc<Notify>>,
  /// Notified once a build session has fetched `GetConfig` (and is in flight).
  entered: Option<Arc<Notify>>,
  /// Exit code reported by the build session; non-zero writes `error_log` to
  /// `console.log` and skips `PushLayer`.
  exit_code: i32,
  /// Text written to `console.log` before exiting.
  console_output: String,
}

/// Drives any build sessions that start (directories under `state/sessions/`),
/// recording every `GetConfigResponse` observed and counting how many build
/// sessions ran. Stops when `stop` is cancelled.
fn spawn_build_responder(
  started_log: PathBuf,
  behavior: Arc<std::sync::Mutex<BuildBehavior>>,
  stop: tokio_util::sync::CancellationToken,
) -> (Arc<AtomicUsize>, Arc<std::sync::Mutex<Vec<GetConfigResponse>>>, tokio::task::JoinHandle<()>) {
  let count = Arc::new(AtomicUsize::new(0));
  let configs = Arc::new(std::sync::Mutex::new(Vec::new()));
  let count_bg = count.clone();
  let configs_bg = configs.clone();

  let handle = tokio::spawn(async move {
    let mut seen = std::collections::HashSet::new();
    while !stop.is_cancelled() {
      if let Ok(text) = std::fs::read_to_string(&started_log) {
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
          let dir = PathBuf::from(line);
          if !seen.insert(dir.clone()) {
            continue;
          }
          // Only drive image build sessions (`state/sessions/<id>`), leaving
          // the final user run (`state/runs/<id>`) parked.
          let is_session = dir.parent().and_then(|p| p.file_name()).is_some_and(|n| n == "sessions");
          if !is_session {
            continue;
          }
          let step_num = count_bg.fetch_add(1, Ordering::SeqCst);
          let configs_task = configs_bg.clone();
          let behavior_task = behavior.clone();
          tokio::spawn(async move {
            let sock = dir.join("v.sock_7000");
            let endpoint = format!("unix:{}", sock.display());
            let client = needy_api_client::Client::connect(&endpoint).await.unwrap();
            let config = client.guest().get_config(GetConfigRequest {}).await.unwrap().into_inner();
            configs_task.lock().unwrap().push(config.clone());

            // Exercise copy_in (e.g. staged Dev Container feature archive) if present.
            for copy in &config.copy_in {
              let mut stream = client
                .guest()
                .fetch_files(needy_api_client::proto::v1::FetchFilesRequest {
                  name: copy.name.clone(),
                })
                .await
                .unwrap()
                .into_inner();
              let mut archive = Vec::new();
              while let Some(chunk) = stream.message().await.unwrap() {
                archive.extend(chunk.data);
              }
              assert!(!archive.is_empty(), "staged copy_in archive `{}` must be non-empty", copy.name);
            }

            let (gate, entered, exit_code, console_output) = {
              let b = behavior_task.lock().unwrap();
              (b.gate.clone(), b.entered.clone(), b.exit_code, b.console_output.clone())
            };
            if let Some(entered) = entered {
              entered.notify_waiters();
            }
            if let Some(gate) = gate {
              gate.notified().await;
            }
            if !console_output.is_empty() {
              std::fs::write(dir.join("console.log"), &console_output).unwrap();
            }
            if exit_code == 0 {
              let script = config.command.last().cloned().unwrap_or_default();
              let archive = tar_with_file(&format!("step-{step_num}.txt"), &format!("{script}\n"));
              needy_api_client::guest::push_layer(&client, std::io::Cursor::new(archive)).await.unwrap();
            }
            client
              .guest()
              .report_exit(ReportExitRequest {
                code: Some(exit_code),
                signal: String::new(),
              })
              .await
              .unwrap();
            std::fs::write(dir.join("done"), "").unwrap();
          });
        }
      }
      tokio::time::sleep(Duration::from_millis(20)).await;
    }
  });
  (count, configs, handle)
}

#[tokio::test]
async fn run_create_builds_missing_run_steps_materializes_disks_and_boots_snapshot() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let started_log = started.dir.path().join("started.log");
  fake_image(&started.dir.path().join("image"), &session_driver_script(&started_log));

  let stop = tokio_util::sync::CancellationToken::new();
  let behavior = Arc::new(std::sync::Mutex::new(BuildBehavior::default()));
  let (sessions, build_configs, responder) = spawn_build_responder(started_log.clone(), behavior, stop.clone());

  let created = tokio::time::timeout(Duration::from_secs(20), Runs::create(daemon, new_run(IMAGE_PROFILE, started.dir.path())))
    .await
    .expect("run create timed out")
    .unwrap();

  // Two build sessions ran, one per `run` layer.
  assert_eq!(sessions.load(Ordering::SeqCst), 2);
  let configs = build_configs.lock().unwrap().clone();
  assert_eq!(configs.len(), 2);

  // Step 1: root user, routing off, upper kept, no lower layers yet.
  assert_eq!(configs[0].user.as_ref().map(|u| (u.name.as_str(), u.uid, u.gid)), Some(("root", 0, 0)));
  assert_eq!(configs[0].command, ["sh", "-c", "echo step-one"]);
  assert!(configs[0].layers.is_empty());
  let sess0 = configs[0].session.unwrap();
  assert_eq!(sess0.upper(), ProtoUpperLayer::Kept);
  assert!(!sess0.routing);

  // Step 2: stacked on top of step 1's built layer.
  assert_eq!(configs[1].command, ["sh", "-c", "echo step-two"]);
  assert_eq!(configs[1].layers.len(), 1);

  // Snapshot digest recorded in RunInfo and persisted in state.db.
  let snapshot_digest = created.snapshot.clone().expect("RunInfo.snapshot must be set");
  assert!(snapshot_digest.starts_with("sha256:"), "{snapshot_digest}");
  let stored = daemon.state.run(&created.id).await.unwrap().unwrap();
  assert_eq!(stored.snapshot.as_deref(), Some(snapshot_digest.as_str()));

  // Final actor's Guest::config receives layers, image_path, merged env, and run session.
  let guest_cfg = Guest::config(daemon, &created.id).await.unwrap();
  assert_eq!(guest_cfg.layers.len(), 2);
  assert_eq!(guest_cfg.layers[0], configs[1].layers[0].digest);
  assert_eq!(guest_cfg.image_path, format!("/opt/tools/bin:{}", needy_core::image::SYSTEM_PATH));
  assert!(!guest_cfg.env.contains_key("PATH"), "{:?}", guest_cfg.env);
  assert_eq!(guest_cfg.env.get("IMAGE_ONLY").map(String::as_str), Some("1"));
  assert_eq!(guest_cfg.env.get("PROFILE_ONLY").map(String::as_str), Some("yes"));
  assert_eq!(guest_cfg.env.get("SHARED").map(String::as_str), Some("from-profile"));
  assert_eq!(
    guest_cfg.session,
    GuestSession {
      upper: UpperLayer::Disposable,
      routing: true,
    }
  );

  // Final actor's vm.json attaches the two materialized SquashFS layer disks after rootfs and needy.
  let run_dir = started.dir.path().join("state/runs").join(&created.id);
  let vm_json: serde_json::Value = serde_json::from_slice(&std::fs::read(run_dir.join("vm.json")).unwrap()).unwrap();
  let drives = vm_json["drives"].as_array().unwrap();
  assert_eq!(drives.len(), 4, "{drives:?}");
  assert_eq!(drives[2]["drive_id"], "layer-0");
  assert_eq!(drives[3]["drive_id"], "layer-1");
  for idx in [2, 3] {
    let path = PathBuf::from(drives[idx]["path_on_host"].as_str().unwrap());
    assert_eq!(&std::fs::read(&path).unwrap()[..4], b"hsqs");
  }

  // Trace records image.build.step events and run.create with snapshot digest; report renders snapshot.
  let stored_events = Runs::events(daemon, &created.id).await.unwrap();
  let decoded: Vec<Recorded> = stored_events
    .events
    .iter()
    .map(|e| Recorded {
      time_ms: e.time_ms,
      event: Event::decode(&e.kind, e.version, &e.payload).unwrap(),
    })
    .collect();
  let build_steps: Vec<_> = decoded
    .iter()
    .filter_map(|r| match &r.event {
      Event::ImageBuildStep(step) => Some(step),
      _ => None,
    })
    .collect();
  assert_eq!(build_steps.len(), 2);
  assert!(!build_steps[0].cached);
  assert!(!build_steps[1].cached);

  let run_create = decoded
    .iter()
    .find_map(|r| match &r.event {
      Event::RunCreate(c) => Some(c),
      _ => None,
    })
    .expect("run.create event missing");
  assert_eq!(run_create.snapshot.as_deref(), Some(snapshot_digest.as_str()));

  let human = report::render(&created.id, &decoded, Level::Summary, Format::Human { color: false });
  assert!(human.contains(&snapshot_digest), "{human}");
  let md = report::render(&created.id, &decoded, Level::Summary, Format::Markdown);
  assert!(md.contains(&snapshot_digest), "{md}");
  let json: serde_json::Value = serde_json::from_str(&report::render(&created.id, &decoded, Level::Summary, Format::Json)).unwrap();
  assert_eq!(json["snapshot"], snapshot_digest);

  Runs::stop(daemon, &created.id, needy_core::types::Principal::Operator, needy_api_server::control::StopMode::Now).await.unwrap();
  stop.cancel();
  responder.await.unwrap();
}

#[tokio::test]
async fn second_run_create_reuses_cached_layers_without_new_build_sessions() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let started_log = started.dir.path().join("started.log");
  fake_image(&started.dir.path().join("image"), &session_driver_script(&started_log));

  let stop = tokio_util::sync::CancellationToken::new();
  let behavior = Arc::new(std::sync::Mutex::new(BuildBehavior::default()));
  let (sessions, _configs, responder) = spawn_build_responder(started_log, behavior, stop.clone());

  let first =
    tokio::time::timeout(Duration::from_secs(20), Runs::create(daemon, new_run(IMAGE_PROFILE, started.dir.path()))).await.unwrap().unwrap();
  assert_eq!(sessions.load(Ordering::SeqCst), 2);

  let second =
    tokio::time::timeout(Duration::from_secs(20), Runs::create(daemon, new_run(IMAGE_PROFILE, started.dir.path()))).await.unwrap().unwrap();
  // Zero additional build sessions ran.
  assert_eq!(sessions.load(Ordering::SeqCst), 2);
  assert_eq!(second.snapshot, first.snapshot);

  // Second run's trace records both build steps as cached.
  let events = Runs::events(daemon, &second.id).await.unwrap();
  let cached: Vec<bool> = events
    .events
    .iter()
    .filter_map(|e| match Event::decode(&e.kind, e.version, &e.payload).unwrap() {
      Event::ImageBuildStep(step) => Some(step.cached),
      _ => None,
    })
    .collect();
  assert_eq!(cached, [true, true]);

  for id in [&first.id, &second.id] {
    Runs::stop(daemon, id, needy_core::types::Principal::Operator, needy_api_server::control::StopMode::Now).await.unwrap();
  }
  stop.cancel();
  responder.await.unwrap();
}

#[tokio::test]
async fn concurrent_run_creates_share_one_build_session_per_cache_key() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let started_log = started.dir.path().join("started.log");
  fake_image(&started.dir.path().join("image"), &session_driver_script(&started_log));

  let single_step_profile = r#"
actor:
  driver: linux-firecracker
  image:
    root: needy-actor:latest
    layers:
      - run: echo shared-step
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  here: { driver: native }
"#;

  let gate = Arc::new(Notify::new());
  let entered = Arc::new(Notify::new());
  let stop = tokio_util::sync::CancellationToken::new();
  let behavior = Arc::new(std::sync::Mutex::new(BuildBehavior {
    gate: Some(gate.clone()),
    entered: Some(entered.clone()),
    ..BuildBehavior::default()
  }));
  let (sessions, _configs, responder) = spawn_build_responder(started_log, behavior, stop.clone());

  let d1 = daemon.clone();
  let d2 = daemon.clone();
  let req1 = new_run(single_step_profile, started.dir.path());
  let req2 = new_run(single_step_profile, started.dir.path());

  let first = tokio::spawn(async move { Runs::create(&d1, req1).await });
  // Wait until the first build session is actively in flight.
  tokio::time::timeout(Duration::from_secs(10), entered.notified()).await.expect("first build session did not start");

  let second = tokio::spawn(async move { Runs::create(&d2, req2).await });
  // Give the second create a moment to join the single-flight waiter before
  // releasing the leader's build session.
  tokio::time::sleep(Duration::from_millis(150)).await;
  gate.notify_waiters();

  let r1 = tokio::time::timeout(Duration::from_secs(20), first).await.unwrap().unwrap().unwrap();
  let r2 = tokio::time::timeout(Duration::from_secs(20), second).await.unwrap().unwrap().unwrap();

  assert_eq!(sessions.load(Ordering::SeqCst), 1, "concurrent creates must share one build session");
  assert!(r1.snapshot.is_some());
  assert_eq!(r1.snapshot, r2.snapshot);

  for id in [&r1.id, &r2.id] {
    Runs::stop(daemon, id, needy_core::types::Principal::Operator, needy_api_server::control::StopMode::Now).await.unwrap();
  }
  stop.cancel();
  responder.await.unwrap();
}

#[tokio::test]
async fn a_failing_build_step_fails_run_create_with_output_and_does_not_cache() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let started_log = started.dir.path().join("started.log");
  fake_image(&started.dir.path().join("image"), &session_driver_script(&started_log));

  let profile = r#"
actor:
  driver: linux-firecracker
  image:
    root: needy-actor:latest
    layers:
      - run: apt-get install -y nonexistent-pkg
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  here: { driver: native }
"#;

  let stop = tokio_util::sync::CancellationToken::new();
  let behavior = Arc::new(std::sync::Mutex::new(BuildBehavior {
    exit_code: 100,
    console_output: "E: Unable to locate package nonexistent-pkg\n".into(),
    ..BuildBehavior::default()
  }));
  let (sessions, _configs, responder) = spawn_build_responder(started_log, behavior.clone(), stop.clone());

  let Err(ControlError::Failed(message)) =
    tokio::time::timeout(Duration::from_secs(20), Runs::create(daemon, new_run(profile, started.dir.path()))).await.unwrap()
  else {
    panic!("a failing build step must fail run create");
  };

  assert!(message.contains("apt-get install -y nonexistent-pkg"), "{message}");
  assert!(message.contains("exited with code 100"), "{message}");
  assert!(message.contains("E: Unable to locate package nonexistent-pkg"), "{message}");
  assert_eq!(sessions.load(Ordering::SeqCst), 1);

  // Because the failed step was not cached, a subsequent create runs a fresh
  // build session and succeeds once the command passes.
  {
    let mut b = behavior.lock().unwrap();
    b.exit_code = 0;
    b.console_output.clear();
  }
  let retried =
    tokio::time::timeout(Duration::from_secs(20), Runs::create(daemon, new_run(profile, started.dir.path()))).await.unwrap().unwrap();
  assert_eq!(sessions.load(Ordering::SeqCst), 2, "failed build must not be cached");
  assert!(retried.snapshot.is_some());

  Runs::stop(daemon, &retried.id, needy_core::types::Principal::Operator, needy_api_server::control::StopMode::Now).await.unwrap();
  stop.cancel();
  responder.await.unwrap();
}

const IMAGE_DEF_YAML: &str = r#"
root: needy-actor:latest
env:
  PATH: /opt/tools/bin:${PATH}
layers:
  - run: echo build-step-1
"#;

#[tokio::test]
async fn image_build_builds_layers_and_returns_snapshot_digest() {
  use needy_api_server::control::{ImageBuild, Images};

  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let started_log = started.dir.path().join("started.log");
  fake_image(&started.dir.path().join("image"), &session_driver_script(&started_log));

  let stop = tokio_util::sync::CancellationToken::new();
  let behavior = Arc::new(std::sync::Mutex::new(BuildBehavior::default()));
  let (sessions, _configs, responder) = spawn_build_responder(started_log, behavior, stop.clone());

  let first = tokio::time::timeout(
    Duration::from_secs(20),
    Images::build(
      daemon,
      ImageBuild {
        image_yaml: IMAGE_DEF_YAML.into(),
        driver: "linux-firecracker".into(),
        user: None,
        commit: None,
        pinned_features: Default::default(),
      },
    ),
  )
  .await
  .unwrap()
  .unwrap();

  assert!(first.snapshot.starts_with("sha256:"), "{}", first.snapshot);
  assert!(!first.commit_rebased);
  assert_eq!(first.steps.len(), 1);
  assert!(!first.steps[0].cached);
  assert_eq!(sessions.load(Ordering::SeqCst), 1);

  // Second build reuses cache.
  let second = Images::build(
    daemon,
    ImageBuild {
      image_yaml: IMAGE_DEF_YAML.into(),
      driver: "linux-firecracker".into(),
      user: None,
      commit: None,
      pinned_features: Default::default(),
    },
  )
  .await
  .unwrap();
  assert_eq!(second.snapshot, first.snapshot);
  assert!(second.steps[0].cached);
  assert_eq!(sessions.load(Ordering::SeqCst), 1);

  stop.cancel();
  responder.await.unwrap();
}

#[tokio::test]
async fn image_edit_routes_all_execs_local_records_commands_commits_refuses_concurrent_and_rolls_back() {
  use needy_api_client::proto::v1::DecideRequest;
  use needy_api_server::control::{ImageEditStart, ImageRollback, Images};

  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let started_log = started.dir.path().join("started.log");
  fake_image(&started.dir.path().join("image"), &session_driver_script(&started_log));

  // Image definition with no `run:` layers so start_edit boots directly into
  // the interactive edit session without build steps.
  let empty_image_yaml = "root: needy-actor:latest\n";

  let edit1 = Images::start_edit(
    daemon,
    ImageEditStart {
      image_name: "test-agent-image".into(),
      image_yaml: empty_image_yaml.into(),
      driver: "linux-firecracker".into(),
      user: None,
      commit: None,
      tty: None,
      wait_for_attach: false,
      pinned_features: Default::default(),
    },
  )
  .await
  .unwrap();
  assert!(!edit1.commit_rebased);

  // Concurrent edit on the same image must be refused.
  let concurrent = Images::start_edit(
    daemon,
    ImageEditStart {
      image_name: "test-agent-image".into(),
      image_yaml: empty_image_yaml.into(),
      driver: "linux-firecracker".into(),
      user: None,
      commit: None,
      tty: None,
      wait_for_attach: false,
      pinned_features: Default::default(),
    },
  )
  .await;
  assert!(matches!(concurrent, Err(ControlError::Failed(ref msg)) if msg.contains("already running")), "{concurrent:?}");

  // Act as the edit session's guest: connect to v.sock_7000, verify config,
  // call Decide for two commands (must be decided local!), push upper.tar, exit 0.
  let session_dir = started.dir.path().join("state/sessions").join(&edit1.session_id);
  let sock = session_dir.join("v.sock_7000");
  for _ in 0..100 {
    if sock.exists() {
      break;
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
  let client = needy_api_client::Client::connect(&format!("unix:{}", sock.display())).await.unwrap();
  let cfg = client.guest().get_config(GetConfigRequest {}).await.unwrap().into_inner();
  assert_eq!(cfg.route_names, ["*"]);
  let sess = cfg.session.unwrap();
  assert_eq!(sess.upper(), ProtoUpperLayer::Kept);
  assert!(sess.routing);

  let d1 = client
    .runs()
    .decide(DecideRequest {
      run_id: edit1.session_id.clone(),
      argv: vec!["apt-get".into(), "install".into(), "-y".into(), "jq".into()],
      cwd: "/".into(),
      ..Default::default()
    })
    .await
    .unwrap()
    .into_inner();
  assert_eq!(d1.target, "actor");

  let d2 = client
    .runs()
    .decide(DecideRequest {
      run_id: edit1.session_id.clone(),
      argv: vec!["touch".into(), "marker".into()],
      cwd: "/opt".into(),
      ..Default::default()
    })
    .await
    .unwrap()
    .into_inner();
  assert_eq!(d2.target, "actor");

  let upper1 = tar_with_file("edit1.txt", "first\n");
  needy_api_client::guest::push_layer(&client, std::io::Cursor::new(upper1)).await.unwrap();
  client
    .guest()
    .report_exit(ReportExitRequest {
      code: Some(0),
      signal: String::new(),
    })
    .await
    .unwrap();
  std::fs::write(session_dir.join("done"), "").unwrap();

  let finished1 =
    tokio::time::timeout(Duration::from_secs(10), Images::finish_edit(daemon, &edit1.session_id, false)).await.unwrap().unwrap();
  let commit1 = finished1.commit.expect("clean edit session must produce a commit");
  assert_eq!(finished1.commands.len(), 2);
  assert_eq!(finished1.commands[0].argv, ["apt-get", "install", "-y", "jq"]);
  assert_eq!(finished1.commands[1].cwd, "/opt");

  // Second edit stacked on commit1, then rollback 1 step -> commit1, then 1 more -> None.
  let edit2 = Images::start_edit(
    daemon,
    ImageEditStart {
      image_name: "test-agent-image".into(),
      image_yaml: empty_image_yaml.into(),
      driver: "linux-firecracker".into(),
      user: None,
      commit: Some(commit1.clone()),
      tty: None,
      wait_for_attach: false,
      pinned_features: Default::default(),
    },
  )
  .await
  .unwrap();
  let session_dir2 = started.dir.path().join("state/sessions").join(&edit2.session_id);
  let sock2 = session_dir2.join("v.sock_7000");
  for _ in 0..100 {
    if sock2.exists() {
      break;
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
  let client2 = needy_api_client::Client::connect(&format!("unix:{}", sock2.display())).await.unwrap();
  let cfg2 = client2.guest().get_config(GetConfigRequest {}).await.unwrap().into_inner();
  assert_eq!(cfg2.layers.len(), 1);
  assert_eq!(cfg2.layers[0].digest, commit1.layer.to_string());
  let upper2 = tar_with_file("edit2.txt", "second\n");
  needy_api_client::guest::push_layer(&client2, std::io::Cursor::new(upper2)).await.unwrap();
  client2
    .guest()
    .report_exit(ReportExitRequest {
      code: Some(0),
      signal: String::new(),
    })
    .await
    .unwrap();
  std::fs::write(session_dir2.join("done"), "").unwrap();

  let finished2 =
    tokio::time::timeout(Duration::from_secs(10), Images::finish_edit(daemon, &edit2.session_id, false)).await.unwrap().unwrap();
  let commit2 = finished2.commit.expect("second edit must produce a commit");
  assert_ne!(commit2.layer, commit1.layer);

  // Roll back 1 step from commit2 -> commit1, restoring commit1's original base.
  let rolled1 = Images::rollback(
    daemon,
    ImageRollback {
      image_name: "test-agent-image".into(),
      from_layer: commit2.layer.to_string(),
      steps: 1,
    },
  )
  .await
  .unwrap();
  assert_eq!(rolled1.layer.as_deref(), Some(commit1.layer.to_string().as_str()));
  assert_eq!(rolled1.base.as_deref(), Some(commit1.base.to_string().as_str()));

  // Roll back 2 steps from commit2 -> None (before first commit).
  let rolled2 = Images::rollback(
    daemon,
    ImageRollback {
      image_name: "test-agent-image".into(),
      from_layer: commit2.layer.to_string(),
      steps: 2,
    },
  )
  .await
  .unwrap();
  assert_eq!(rolled2.layer, None);
  assert_eq!(rolled2.base, None);
}

#[tokio::test]
async fn run_create_with_lockfile_commit_stacks_commit_layer_and_reports_rebase() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;

  let profile = r#"
actor:
  driver: linux-firecracker
  image:
    root: needy-actor:latest
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  here: { driver: native }
"#;

  // Store a commit layer in the daemon's layer store.
  let commit_layer =
    daemon.images.store.commit("test-img", None, None, std::io::Cursor::new(tar_with_file("hand.txt", "edited\n")), Vec::new()).unwrap();
  let root = crate::image::root_digest(daemon).await.unwrap();
  let def = needy_config::image::Definition {
    root: Some("needy-actor:latest".into()),
    ..Default::default()
  };
  let expected_top =
    *needy_core::image::plan(&def, &root, &needy_config::profile::User::default(), None, &std::collections::BTreeMap::new()).unwrap().top();

  // Matching base -> commit_rebased == false, commit layer stacked on top.
  let mut req = new_run(profile, started.dir.path());
  req.image_commit = Some(needy_core::image::Commit {
    layer: commit_layer,
    base: expected_top,
  });
  let created = Runs::create(daemon, req).await.unwrap();
  assert!(!created.commit_rebased);
  let cfg = Guest::config(daemon, &created.id).await.unwrap();
  assert_eq!(cfg.layers, [commit_layer.to_string()]);
  Runs::stop(daemon, &created.id, needy_core::types::Principal::Operator, needy_api_server::control::StopMode::Now).await.unwrap();

  // Different base -> commit_rebased == true, commit layer still stacked on top.
  let stale_base = needy_core::image::Digest::parse(&format!("sha256:{}", "0".repeat(64))).unwrap();
  let mut req_rebased = new_run(profile, started.dir.path());
  req_rebased.image_commit = Some(needy_core::image::Commit {
    layer: commit_layer,
    base: stale_base,
  });
  let rebased = Runs::create(daemon, req_rebased).await.unwrap();
  assert!(rebased.commit_rebased);
  let cfg2 = Guest::config(daemon, &rebased.id).await.unwrap();
  assert_eq!(cfg2.layers, [commit_layer.to_string()]);
  Runs::stop(daemon, &rebased.id, needy_core::types::Principal::Operator, needy_api_server::control::StopMode::Now).await.unwrap();
}

// ROOT CAUSE:
//
// If a locally built base VM image had no `manifest.sha256`, `root_digest`
// hashed the entire `rootfs.ext4` (hundreds of MiB) on every `run create`,
// `image build`, and `image edit`, adding ~17 s per call in debug builds.
//
// Before the fix, every call re-read and hashed `rootfs.ext4` from scratch.
// The fix caches the digest in `Images` keyed by `(path, len, modified)` and
// recomputes it when the file changes.
#[tokio::test]
async fn root_digest_caches_unhashed_rootfs_and_invalidates_on_change() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;

  let first = crate::image::root_digest(daemon).await.unwrap();
  let second = crate::image::root_digest(daemon).await.unwrap();
  assert_eq!(first, second);

  // Replace rootfs.ext4 with different length -> cache must invalidate.
  std::fs::write(daemon.config.actor_image.join("rootfs.ext4"), b"updated-rootfs-contents").unwrap();
  let third = crate::image::root_digest(daemon).await.unwrap();
  assert_ne!(first, third);
}

/// Builds a minimal valid Docker-save (`manifest.json`) image archive with the
/// given layers (each layer is a slice of `(path, content)` pairs).
fn fake_oci_image_tar(tag: &str, layers: &[&[(&str, &[u8])]]) -> Vec<u8> {
  use sha2::Digest as _;
  let mut layer_tars: Vec<(String, Vec<u8>)> = Vec::new();
  for files in layers {
    let mut builder = tar::Builder::new(Vec::new());
    for (path, bytes) in *files {
      let mut header = tar::Header::new_gnu();
      header.set_entry_type(tar::EntryType::Regular);
      header.set_mode(0o755);
      header.set_uid(0);
      header.set_gid(0);
      header.set_mtime(0);
      header.set_size(bytes.len() as u64);
      header.set_cksum();
      builder.append_data(&mut header, path, *bytes).unwrap();
    }
    let bytes = builder.into_inner().unwrap();
    let digest = format!("sha256:{:x}", sha2::Sha256::digest(&bytes));
    layer_tars.push((digest, bytes));
  }

  let diff_ids: Vec<String> = layer_tars.iter().map(|(d, _)| d.clone()).collect();
  let config_json = serde_json::json!({
    "architecture": needy_core_actor::image::container::oci_arch(needy_core_actor::image::guest::arch()),
    "os": "linux",
    "rootfs": { "type": "layers", "diff_ids": diff_ids },
    "config": { "Labels": { "needy.base-abi": "1" } },
  });
  let config_bytes = serde_json::to_vec(&config_json).unwrap();
  let config_hex = format!("{:x}", sha2::Sha256::digest(&config_bytes));
  let config_path = format!("blobs/sha256/{config_hex}");

  let layer_paths: Vec<String> = layer_tars.iter().map(|(d, _)| format!("blobs/sha256/{}", d.strip_prefix("sha256:").unwrap())).collect();
  let manifest_json = serde_json::json!([{
    "Config": config_path,
    "RepoTags": [tag],
    "Layers": layer_paths,
  }]);
  let manifest_bytes = serde_json::to_vec(&manifest_json).unwrap();

  let mut archive = tar::Builder::new(Vec::new());
  let append = |archive: &mut tar::Builder<Vec<u8>>, name: &str, bytes: &[u8]| {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_mode(0o644);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_size(bytes.len() as u64);
    header.set_cksum();
    archive.append_data(&mut header, name, bytes).unwrap();
  };
  append(&mut archive, "manifest.json", &manifest_bytes);
  append(&mut archive, &config_path, &config_bytes);
  for (path, (_, bytes)) in layer_paths.iter().zip(&layer_tars) {
    append(&mut archive, path, bytes);
  }
  archive.into_inner().unwrap()
}

/// Extracts `RepoTags[0]` from a Docker-save / hybrid OCI tar archive.
fn repo_tag_from_tar(bytes: &[u8]) -> String {
  use std::io::Read as _;
  let mut archive = tar::Archive::new(std::io::Cursor::new(bytes));
  for entry in archive.entries().unwrap() {
    let mut entry = entry.unwrap();
    if entry.path().unwrap().to_string_lossy() == "manifest.json" {
      let mut json = Vec::new();
      entry.read_to_end(&mut json).unwrap();
      let v: serde_json::Value = serde_json::from_slice(&json).unwrap();
      return v[0]["RepoTags"][0].as_str().unwrap().to_string();
    }
  }
  panic!("manifest.json not found in loaded OCI archive");
}

type OnContainerStart =
  Arc<dyn Fn(String, needy_core_executor::engine::models::ContainerCreateBody, tokio::sync::watch::Sender<bool>) + Send + Sync>;

struct FakeContainerEngine {
  images: std::sync::Mutex<std::collections::HashMap<String, (needy_core_executor::engine::Image, Vec<u8>)>>,
  created: std::sync::Mutex<Vec<(String, needy_core_executor::engine::models::ContainerCreateBody)>>,
  exported: std::sync::Mutex<Vec<String>>,
  loaded: std::sync::Mutex<Vec<String>>,
  committed: std::sync::Mutex<Vec<(String, String)>>,
  removed_images: std::sync::Mutex<Vec<String>>,
  removed_containers: std::sync::Mutex<Vec<String>>,
  wait_gates: std::sync::Mutex<std::collections::HashMap<String, tokio::sync::watch::Sender<bool>>>,
  on_start: Option<OnContainerStart>,
  base_tar: Vec<u8>,
}

impl FakeContainerEngine {
  fn new(base_tag: &str, on_start: Option<OnContainerStart>) -> Arc<Self> {
    let base_tar = fake_oci_image_tar(base_tag, &[&[(".needy/proxy", b"fake-proxy")]]);
    let image = needy_core_executor::engine::Image {
      id: format!("sha256:{}", "a".repeat(64)),
      os: "linux".into(),
      architecture: needy_core_actor::image::container::oci_arch(needy_core_actor::image::guest::arch()).into(),
      labels: std::collections::HashMap::from([("needy.base-abi".into(), "1".into())]),
    };
    let mut images = std::collections::HashMap::new();
    images.insert(base_tag.to_string(), (image, base_tar.clone()));
    Arc::new(Self {
      images: std::sync::Mutex::new(images),
      created: Default::default(),
      exported: Default::default(),
      loaded: Default::default(),
      committed: Default::default(),
      removed_images: Default::default(),
      removed_containers: Default::default(),
      wait_gates: Default::default(),
      on_start,
      base_tar,
    })
  }
}

#[async_trait::async_trait]
impl needy_core_executor::engine::Engine for FakeContainerEngine {
  async fn runtimes(&self) -> std::result::Result<std::collections::BTreeMap<String, Vec<String>>, needy_core_executor::engine::Error> {
    Ok(std::collections::BTreeMap::from([("runc".into(), Vec::new())]))
  }

  async fn inspect_image(
    &self,
    image: &str,
  ) -> std::result::Result<Option<needy_core_executor::engine::Image>, needy_core_executor::engine::Error> {
    Ok(self.images.lock().unwrap().get(image).map(|(img, _)| img.clone()))
  }

  fn pull_progress(
    &self,
    image: &str,
  ) -> futures_util::stream::BoxStream<
    'static,
    std::result::Result<needy_core_executor::engine::models::CreateImageInfo, needy_core_executor::engine::Error>,
  > {
    use futures_util::StreamExt as _;
    futures_util::stream::iter([Err(needy_core_executor::engine::Error::NotFound(
      image.into(),
    ))])
    .boxed()
  }

  async fn create(
    &self,
    name: &str,
    body: needy_core_executor::engine::models::ContainerCreateBody,
  ) -> std::result::Result<(), needy_core_executor::engine::Error> {
    self.created.lock().unwrap().push((name.to_string(), body));
    let (tx, _) = tokio::sync::watch::channel(false);
    self.wait_gates.lock().unwrap().insert(name.to_string(), tx);
    Ok(())
  }

  async fn start(&self, name: &str) -> std::result::Result<u32, needy_core_executor::engine::Error> {
    let gate = self.wait_gates.lock().unwrap().get(name).cloned().unwrap();
    let body = self.created.lock().unwrap().iter().rev().find(|(n, _)| n == name).unwrap().1.clone();
    if let Some(cb) = &self.on_start {
      cb(name.to_string(), body, gate);
    }
    Ok(4242)
  }

  fn logs(
    &self,
    _name: &str,
  ) -> futures_util::stream::BoxStream<'static, std::result::Result<bytes::Bytes, needy_core_executor::engine::Error>> {
    use futures_util::StreamExt as _;
    futures_util::stream::empty().boxed()
  }

  async fn wait(&self, name: &str) -> std::result::Result<i64, needy_core_executor::engine::Error> {
    let rx = self.wait_gates.lock().unwrap().get(name).map(|tx| tx.subscribe());
    if let Some(mut rx) = rx {
      let _ = rx.wait_for(|done| *done).await;
    }
    Ok(0)
  }

  async fn stop(&self, name: &str, _grace: Duration) -> std::result::Result<(), needy_core_executor::engine::Error> {
    if let Some(gate) = self.wait_gates.lock().unwrap().get(name) {
      gate.send_replace(true);
    }
    Ok(())
  }

  async fn remove(&self, name: &str) -> std::result::Result<(), needy_core_executor::engine::Error> {
    self.removed_containers.lock().unwrap().push(name.to_string());
    if let Some(gate) = self.wait_gates.lock().unwrap().get(name) {
      gate.send_replace(true);
    }
    Ok(())
  }

  async fn download(&self, _name: &str, path: &str) -> std::result::Result<Option<Vec<u8>>, needy_core_executor::engine::Error> {
    if path != "/etc/passwd" {
      return Ok(None);
    }
    let text = b"root:x:0:0:root:/root:/bin/sh\nagent:x:1000:1000::/home/agent:/bin/sh\n";
    let mut archive = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(text.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    archive.append_data(&mut header, "passwd", &text[..]).unwrap();
    Ok(Some(archive.into_inner().unwrap()))
  }

  async fn commit_container(
    &self,
    container: &str,
    repo: &str,
    tag: &str,
  ) -> std::result::Result<String, needy_core_executor::engine::Error> {
    let full = format!("{repo}:{tag}");
    self.committed.lock().unwrap().push((container.to_string(), full.clone()));
    let committed_tar = fake_oci_image_tar(
      &full,
      &[
        &[(".needy/proxy", b"fake-proxy")],
        &[("etc/committed.txt", format!("committed-from-{container}\n").as_bytes())],
      ],
    );
    let image = needy_core_executor::engine::Image {
      id: format!("sha256:{}", "c".repeat(64)),
      os: "linux".into(),
      architecture: needy_core_actor::image::container::oci_arch(needy_core_actor::image::guest::arch()).into(),
      labels: std::collections::HashMap::from([("needy.base-abi".into(), "1".into())]),
    };
    self.images.lock().unwrap().insert(full, (image, committed_tar));
    Ok(format!("sha256:{}", "c".repeat(64)))
  }

  fn export_image(
    &self,
    image: &str,
  ) -> futures_util::stream::BoxStream<'static, std::result::Result<bytes::Bytes, needy_core_executor::engine::Error>> {
    use futures_util::StreamExt as _;
    self.exported.lock().unwrap().push(image.to_string());
    let bytes = self.images.lock().unwrap().get(image).map(|(_, tar)| tar.clone()).unwrap_or_else(|| self.base_tar.clone());
    futures_util::stream::iter([Ok(bytes::Bytes::from(bytes))]).boxed()
  }

  async fn load_image(
    &self,
    mut tar: futures_util::stream::BoxStream<'static, std::result::Result<bytes::Bytes, needy_core_executor::engine::Error>>,
  ) -> std::result::Result<(), needy_core_executor::engine::Error> {
    use futures_util::StreamExt as _;
    let mut all = Vec::new();
    while let Some(chunk) = tar.next().await {
      all.extend_from_slice(&chunk?);
    }
    let tag = repo_tag_from_tar(&all);
    self.loaded.lock().unwrap().push(tag.clone());
    let image = needy_core_executor::engine::Image {
      id: format!("sha256:{}", "b".repeat(64)),
      os: "linux".into(),
      architecture: needy_core_actor::image::container::oci_arch(needy_core_actor::image::guest::arch()).into(),
      labels: std::collections::HashMap::from([("needy.base-abi".into(), "1".into())]),
    };
    self.images.lock().unwrap().insert(tag, (image, all));
    Ok(())
  }

  async fn remove_image(&self, image: &str) -> std::result::Result<(), needy_core_executor::engine::Error> {
    self.removed_images.lock().unwrap().push(image.to_string());
    self.images.lock().unwrap().remove(image);
    Ok(())
  }
}

struct FakeAppleCli {
  images: std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>,
  saved: std::sync::Mutex<Vec<String>>,
  loaded: std::sync::Mutex<Vec<String>>,
  spawned_args: std::sync::Mutex<Vec<Vec<String>>>,
  wait_gates: std::sync::Mutex<std::collections::HashMap<String, tokio::sync::watch::Sender<bool>>>,
}

impl FakeAppleCli {
  fn new(base_tag: &str) -> Arc<Self> {
    let base_tar = fake_oci_image_tar(base_tag, &[&[(".needy/proxy", b"fake-proxy")]]);
    let mut images = std::collections::HashMap::new();
    images.insert(base_tag.to_string(), base_tar);
    Arc::new(Self {
      images: std::sync::Mutex::new(images),
      saved: Default::default(),
      loaded: Default::default(),
      spawned_args: Default::default(),
      wait_gates: Default::default(),
    })
  }
}

struct ParkedAppleProcess(tokio::sync::watch::Sender<bool>);

#[async_trait::async_trait]
impl needy_core_actor::driver::macos_apple_container::Process for ParkedAppleProcess {
  fn id(&self) -> u32 {
    5151
  }
  async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
    use std::os::unix::process::ExitStatusExt as _;
    let mut rx = self.0.subscribe();
    let _ = rx.wait_for(|done| *done).await;
    Ok(std::process::ExitStatus::from_raw(0))
  }
  fn kill(&mut self) -> std::io::Result<()> {
    self.0.send_replace(true);
    Ok(())
  }
}

#[async_trait::async_trait]
impl needy_core_actor::driver::macos_apple_container::Cli for FakeAppleCli {
  async fn output(
    &self,
    args: &[String],
  ) -> std::result::Result<needy_core_actor::driver::macos_apple_container::Output, needy_core_actor::driver::Error> {
    use needy_core_actor::driver::macos_apple_container::Output;
    let slice: Vec<&str> = args.iter().map(String::as_str).collect();
    match slice.as_slice() {
      ["system", "status"] => Ok(Output {
        success: true,
        stdout: String::new(),
        stderr: String::new(),
      }),
      ["delete", "--force", name] | ["stop", "--time", _, name] => {
        if let Some(tx) = self.wait_gates.lock().unwrap().get(*name) {
          tx.send_replace(true);
        }
        Ok(Output {
          success: true,
          stdout: String::new(),
          stderr: String::new(),
        })
      }
      ["image", "inspect", image] => {
        if self.images.lock().unwrap().contains_key(*image) {
          Ok(Output {
            success: true,
            stdout: format!(
              r#"[{{"configuration": {{"name": "{image}"}}, "id": "applebase001", "variants": [{{"config": {{"architecture": "arm64", "os": "linux", "config": {{"Labels": {{"needy.base-abi": "1"}}}}}}}}]}}]"#
            ),
            stderr: String::new(),
          })
        } else {
          Ok(Output {
            success: false,
            stdout: String::new(),
            stderr: "Error: not found\n".into(),
          })
        }
      }
      ["image", "save", "--output", path, image] => {
        let tar = self.images.lock().unwrap().get(*image).cloned().unwrap();
        std::fs::write(path, tar).unwrap();
        self.saved.lock().unwrap().push((*image).to_string());
        Ok(Output {
          success: true,
          stdout: String::new(),
          stderr: String::new(),
        })
      }
      ["image", "load", "--input", path] => {
        let bytes = std::fs::read(path).unwrap();
        let tag = repo_tag_from_tar(&bytes);
        self.images.lock().unwrap().insert(tag.clone(), bytes);
        self.loaded.lock().unwrap().push(tag);
        Ok(Output {
          success: true,
          stdout: String::new(),
          stderr: String::new(),
        })
      }
      other => panic!("unexpected Apple container CLI call: {other:?}"),
    }
  }

  fn spawn(
    &self,
    args: &[String],
    _log: std::fs::File,
  ) -> std::result::Result<Box<dyn needy_core_actor::driver::macos_apple_container::Process>, needy_core_actor::driver::Error> {
    self.spawned_args.lock().unwrap().push(args.to_vec());
    let (tx, _) = tokio::sync::watch::channel(false);
    if let Some(name) = args.windows(2).find(|w| w[0] == "--name").map(|w| w[1].clone()) {
      self.wait_gates.lock().unwrap().insert(name, tx.clone());
    }
    Ok(Box::new(ParkedAppleProcess(tx)))
  }
}

#[tokio::test]
async fn run_create_on_linux_container_materializes_loads_and_boots_snapshot_image() {
  let engine = FakeContainerEngine::new("needy-actor:latest", None);
  let backends = crate::holder::ContainerBackends {
    engine: engine.clone(),
    apple_cli: needy_core_actor::driver::macos_apple_container::default_cli(),
  };
  let started = testing::daemon_in_with_backends(tempfile::tempdir_in("/tmp").unwrap(), backends, |_| {}).await;
  let daemon = &started.daemon;
  let started_log = started.dir.path().join("started.log");
  fake_image(&started.dir.path().join("image"), &session_driver_script(&started_log));

  let stop = tokio_util::sync::CancellationToken::new();
  let behavior = Arc::new(std::sync::Mutex::new(BuildBehavior::default()));
  let (sessions, _configs, responder) = spawn_build_responder(started_log, behavior, stop.clone());

  let profile = r#"
actor:
  driver: linux-container
  container: { oci_runtime: runc }
  image:
    root: needy-actor:latest
    layers:
      - run: echo container-step
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  here: { driver: native }
"#;

  let created = tokio::time::timeout(Duration::from_secs(20), Runs::create(daemon, new_run(profile, started.dir.path())))
    .await
    .expect("run create timed out")
    .unwrap();

  assert_eq!(sessions.load(Ordering::SeqCst), 1, "build step ran on the host's VM builder");
  let snapshot = created.snapshot.clone().expect("snapshot digest recorded");
  let expected_tag = format!("needy-snapshot:{}", snapshot.strip_prefix("sha256:").unwrap());

  assert_eq!(*engine.exported.lock().unwrap(), vec!["needy-actor:latest"]);
  assert_eq!(*engine.loaded.lock().unwrap(), vec![expected_tag.clone()]);

  let actor_body = engine
    .created
    .lock()
    .unwrap()
    .iter()
    .find(|(name, _)| name == &format!("needy-actor-{}", created.id))
    .expect("actor container created")
    .1
    .clone();
  assert_eq!(actor_body.image.as_deref(), Some(expected_tag.as_str()));

  // Container runs boot the assembled OCI image, so GuestConfig.layers is empty.
  let guest_cfg = Guest::config(daemon, &created.id).await.unwrap();
  assert!(guest_cfg.layers.is_empty(), "{:?}", guest_cfg.layers);

  // Second create reuses cached base export and already-loaded snapshot tag.
  let second =
    tokio::time::timeout(Duration::from_secs(20), Runs::create(daemon, new_run(profile, started.dir.path()))).await.unwrap().unwrap();
  assert_eq!(second.snapshot, created.snapshot);
  assert_eq!(engine.exported.lock().unwrap().len(), 1, "base export must be cached");
  assert_eq!(engine.loaded.lock().unwrap().len(), 1, "already-loaded snapshot must not be reloaded");

  for id in [&created.id, &second.id] {
    Runs::stop(daemon, id, needy_core::types::Principal::Operator, needy_api_server::control::StopMode::Now).await.unwrap();
  }
  stop.cancel();
  responder.await.unwrap();
}

#[tokio::test]
async fn run_create_on_macos_apple_container_saves_base_materializes_loads_and_boots_snapshot_image() {
  let apple_cli = FakeAppleCli::new("needy-actor:latest");
  let backends = crate::holder::ContainerBackends {
    engine: needy_core_executor::engine::local(),
    apple_cli: apple_cli.clone(),
  };
  let started = testing::daemon_in_with_backends(tempfile::tempdir_in("/tmp").unwrap(), backends, |_| {}).await;
  let daemon = &started.daemon;
  let started_log = started.dir.path().join("started.log");
  fake_image(&started.dir.path().join("image"), &session_driver_script(&started_log));

  let stop = tokio_util::sync::CancellationToken::new();
  let behavior = Arc::new(std::sync::Mutex::new(BuildBehavior::default()));
  let (sessions, _configs, responder) = spawn_build_responder(started_log, behavior, stop.clone());

  let profile = r#"
actor:
  driver: macos-apple-container
  image:
    root: needy-actor:latest
    layers:
      - run: echo apple-step
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  here: { driver: native }
"#;

  let created = tokio::time::timeout(Duration::from_secs(20), Runs::create(daemon, new_run(profile, started.dir.path())))
    .await
    .expect("run create timed out")
    .unwrap();

  assert_eq!(sessions.load(Ordering::SeqCst), 1, "Apple's container is never used to build");
  let snapshot = created.snapshot.clone().expect("snapshot digest recorded");
  let expected_tag = format!("needy-snapshot:{}", snapshot.strip_prefix("sha256:").unwrap());

  assert_eq!(*apple_cli.saved.lock().unwrap(), vec!["needy-actor:latest"]);
  assert_eq!(*apple_cli.loaded.lock().unwrap(), vec![expected_tag.clone()]);

  let spawned = apple_cli.spawned_args.lock().unwrap().clone();
  assert_eq!(spawned.len(), 1);
  assert_eq!(spawned[0].last().map(String::as_str), Some(expected_tag.as_str()));

  let guest_cfg = Guest::config(daemon, &created.id).await.unwrap();
  assert!(guest_cfg.layers.is_empty(), "{:?}", guest_cfg.layers);

  Runs::stop(daemon, &created.id, needy_core::types::Principal::Operator, needy_api_server::control::StopMode::Now).await.unwrap();
  stop.cancel();
  responder.await.unwrap();
}

#[tokio::test]
async fn linux_container_fallback_builds_and_edits_without_vm_driver_and_cleans_up_temp_image() {
  use needy_api_client::proto::v1::DecideRequest;
  use needy_api_server::control::{ImageBuild, ImageEditStart, Images};

  // Simulate each container session's guest connecting to its bind-mounted
  // `channels/guest.sock`, fetching config, (for edit sessions) calling Decide,
  // reporting exit 0, and unblocking `wait()`.
  let on_start: OnContainerStart = Arc::new(|name, body, gate| {
    tokio::spawn(async move {
      let binds = body.host_config.and_then(|hc| hc.binds).unwrap_or_default();
      let channels_host = binds.iter().find_map(|b| b.strip_suffix(":/.needy/channels")).expect("channels bind mount missing").to_string();
      let sock = PathBuf::from(channels_host).join("guest.sock");
      let client = needy_api_client::Client::connect(&format!("unix:{}", sock.display())).await.unwrap();
      let cfg = client.guest().get_config(GetConfigRequest {}).await.unwrap().into_inner();
      assert!(cfg.layers.is_empty(), "container sessions boot OCI images, not VM layer disks");
      if name.starts_with("needy-actor-e-") {
        client
          .runs()
          .decide(DecideRequest {
            argv: vec!["touch".into(), "/etc/edited".into()],
            cwd: "/".into(),
            ..Default::default()
          })
          .await
          .unwrap();
      }
      client
        .guest()
        .report_exit(ReportExitRequest {
          code: Some(0),
          signal: String::new(),
        })
        .await
        .unwrap();
      gate.send_replace(true);
    });
  });

  let engine = FakeContainerEngine::new("needy-actor:latest", Some(on_start));
  let backends = crate::holder::ContainerBackends {
    engine: engine.clone(),
    apple_cli: needy_core_actor::driver::macos_apple_container::default_cli(),
  };
  let started = testing::daemon_in_with_backends(tempfile::tempdir_in("/tmp").unwrap(), backends, |config| {
    // Remove the fake KVM device so no VM driver is available and the daemon
    // falls back to `linux-container` for builds and edit sessions.
    std::fs::remove_file(&config.kvm).unwrap();
  })
  .await;
  let daemon = &started.daemon;

  let image_yaml = "root: needy-actor:latest\nlayers:\n  - run: echo step-1\n  - run: echo step-2\n";
  let built = tokio::time::timeout(
    Duration::from_secs(20),
    Images::build(
      daemon,
      ImageBuild {
        image_yaml: image_yaml.into(),
        driver: "linux-container".into(),
        user: None,
        commit: None,
        pinned_features: Default::default(),
      },
    ),
  )
  .await
  .expect("container build timed out")
  .unwrap();

  assert_eq!(built.steps.len(), 2);
  assert!(!built.steps[0].cached);
  assert!(!built.steps[1].cached);
  // Both build sessions were committed, their temporary images removed, and
  // their containers removed.
  let commits = engine.committed.lock().unwrap().clone();
  assert_eq!(commits.len(), 2, "{commits:?}");
  let removed_imgs = engine.removed_images.lock().unwrap().clone();
  assert_eq!(removed_imgs, vec![commits[0].1.clone(), commits[1].1.clone()]);
  // Every build container ran with a writable rootfs (`readonly_rootfs == false`).
  let created = engine.created.lock().unwrap().clone();
  assert_eq!(created.len(), 2);
  assert_eq!(created[0].1.image.as_deref(), Some("needy-actor:latest"));
  assert_eq!(created[0].1.host_config.as_ref().unwrap().readonly_rootfs, Some(false));
  assert!(
    created[1].1.image.as_deref().is_some_and(|i| i.starts_with("needy-snapshot:")),
    "step 2 must boot from step 1's materialized snapshot image: {:?}",
    created[1].1.image
  );

  // Interactive edit session also runs on `linux-container` fallback, records
  // commands, commits the top layer, and cleans up the temporary image.
  let edit = Images::start_edit(
    daemon,
    ImageEditStart {
      image_name: "fallback-img".into(),
      image_yaml: image_yaml.into(),
      driver: "linux-container".into(),
      user: None,
      commit: None,
      tty: None,
      wait_for_attach: false,
      pinned_features: Default::default(),
    },
  )
  .await
  .unwrap();

  let finished = tokio::time::timeout(Duration::from_secs(20), Images::finish_edit(daemon, &edit.session_id, false))
    .await
    .expect("container edit timed out")
    .unwrap();
  let commit = finished.commit.expect("container edit must produce a commit");
  assert!(daemon.images.store.has_layer(&commit.layer.to_string().parse().unwrap()));
  assert_eq!(finished.commands.len(), 1);
  assert_eq!(finished.commands[0].argv, ["touch", "/etc/edited"]);
  assert_eq!(engine.committed.lock().unwrap().len(), 3);
  assert_eq!(engine.removed_images.lock().unwrap().len(), 3);
}

// ------------------------------------------------------------ feature builds

struct MockFeatureArtifact {
  repo: String,
  tag: String,
  manifest_bytes: Vec<u8>,
  manifest_digest: needy_core::image::Digest,
  blob_digest: needy_core::image::Digest,
  blob_bytes: Vec<u8>,
}

fn make_feature_artifact(repo: &str, tag: &str, metadata_json: &str, install_sh: &str) -> MockFeatureArtifact {
  let mut tar = tar::Builder::new(Vec::new());
  for (name, content) in [
    ("devcontainer-feature.json", metadata_json.as_bytes()),
    ("install.sh", install_sh.as_bytes()),
  ] {
    let mut header = tar::Header::new_gnu();
    header.set_size(content.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, name, content).unwrap();
  }
  let blob_bytes = tar.into_inner().unwrap();
  let blob_digest = needy_core::image::Digest::of(&blob_bytes);
  let manifest_bytes = serde_json::to_vec(&serde_json::json!({
    "schemaVersion": 2,
    "mediaType": "application/vnd.oci.image.manifest.v1+json",
    "config": {
      "mediaType": "application/vnd.devcontainers",
      "digest": needy_core::image::Digest::of(b"{}").to_string(),
      "size": 2
    },
    "layers": [
      {
        "mediaType": "application/vnd.devcontainers.layer.v1+tar",
        "digest": blob_digest.to_string(),
        "size": blob_bytes.len()
      }
    ]
  }))
  .unwrap();
  let manifest_digest = needy_core::image::Digest::of(&manifest_bytes);
  MockFeatureArtifact {
    repo: repo.to_string(),
    tag: tag.to_string(),
    manifest_bytes,
    manifest_digest,
    blob_digest,
    blob_bytes,
  }
}

/// Spawns a local HTTP/1.1 mock OCI registry serving multiple feature
/// repositories and counting every incoming HTTP request.
async fn spawn_feature_registry(artifacts: Vec<MockFeatureArtifact>) -> (String, Arc<AtomicUsize>) {
  use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  let base_url = format!("http://{addr}");
  let requests = Arc::new(AtomicUsize::new(0));
  let requests_bg = requests.clone();
  let artifacts = Arc::new(artifacts);

  tokio::spawn(async move {
    while let Ok((mut stream, _)) = listener.accept().await {
      let requests = requests_bg.clone();
      let artifacts = artifacts.clone();
      tokio::spawn(async move {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
          match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
          }
        }
        requests.fetch_add(1, Ordering::SeqCst);

        let raw = String::from_utf8_lossy(&buf);
        let target = raw.lines().next().unwrap_or("").split_whitespace().nth(1).unwrap_or("/");

        let mut response = ("404 Not Found", String::new(), b"not found".to_vec());
        for a in artifacts.iter() {
          let manifest_tag = format!("/v2/{}/manifests/{}", a.repo, a.tag);
          let manifest_digest = format!("/v2/{}/manifests/{}", a.repo, a.manifest_digest);
          let blob_path = format!("/v2/{}/blobs/{}", a.repo, a.blob_digest);
          if target == manifest_tag || target == manifest_digest {
            response = (
              "200 OK",
              format!("Content-Type: application/vnd.oci.image.manifest.v1+json\r\nDocker-Content-Digest: {}\r\n", a.manifest_digest),
              a.manifest_bytes.clone(),
            );
            break;
          } else if target == blob_path {
            response = ("200 OK", "Content-Type: application/vnd.devcontainers.layer.v1+tar\r\n".to_string(), a.blob_bytes.clone());
            break;
          }
        }

        let (status, headers, body) = response;
        let head = format!("HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len());
        if stream.write_all(head.as_bytes()).await.is_ok() {
          let _ = stream.write_all(&body).await;
        }
      });
    }
  });

  (base_url, requests)
}

#[tokio::test]
async fn feature_layer_with_depends_on_fetches_executes_in_order_merges_env_and_returns_resolved_features() {
  use needy_core_actor::image::feature::FeatureStore;

  let common = make_feature_artifact(
    "devcontainers/features/common",
    "1",
    r#"{
      "id": "common",
      "version": "1.0.0",
      "options": {
        "greeting": { "type": "string", "default": "hello" }
      },
      "containerEnv": {
        "COMMON_DIR": "/opt/common",
        "PATH": "${COMMON_DIR}/bin:${PATH}"
      }
    }"#,
    "#!/bin/sh\necho installing common\n",
  );
  let node = make_feature_artifact(
    "devcontainers/features/node",
    "1",
    r#"{
      "id": "node",
      "version": "1.5.0",
      "options": {
        "version": { "type": "string", "default": "lts" },
        "node-gyp-dependencies": { "type": "boolean", "default": true }
      },
      "dependsOn": {
        "ghcr.io/devcontainers/features/common:1": { "greeting": "hi" }
      },
      "containerEnv": {
        "NODE_HOME": "/usr/local/share/nvm",
        "PATH": "${NODE_HOME}/bin:${PATH}"
      }
    }"#,
    "#!/bin/sh\necho installing node\n",
  );
  let common_digest = common.manifest_digest.to_string();
  let node_digest = node.manifest_digest.to_string();

  let (base_url, requests) = spawn_feature_registry(vec![common, node]).await;

  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let feature_store = FeatureStore::new(&daemon.config.cache_dir).unwrap().without_proxy().unwrap().with_base_url(&base_url);
  daemon.images.set_feature_store(feature_store);

  let started_log = started.dir.path().join("started.log");
  fake_image(&started.dir.path().join("image"), &session_driver_script(&started_log));

  let stop = tokio_util::sync::CancellationToken::new();
  let behavior = Arc::new(std::sync::Mutex::new(BuildBehavior::default()));
  let (sessions, build_configs, responder) = spawn_build_responder(started_log, behavior, stop.clone());

  let profile = r#"
actor:
  driver: linux-firecracker
  image:
    root: needy-actor:latest
    layers:
      - feature: ghcr.io/devcontainers/features/node:1
        with:
          version: "22"
user: { name: dev, uid: 1001, gid: 1001 }
executors:
  here: { driver: native }
commands:
  - { match: "**", to: here }
"#;

  let created = tokio::time::timeout(Duration::from_secs(20), Runs::create(daemon, new_run(profile, started.dir.path())))
    .await
    .expect("run create with feature timed out")
    .unwrap();

  // Both features ran as build steps, in dependency order (`common` then `node`).
  assert_eq!(sessions.load(Ordering::SeqCst), 2);
  assert!(requests.load(Ordering::SeqCst) > 0);
  let configs = build_configs.lock().unwrap().clone();
  assert_eq!(configs.len(), 2);

  // Step 0 (`common`): runs as root, stages feature archive via copy_in,
  // exports _REMOTE_USER=dev, _REMOTE_USER_HOME=/home/dev, _CONTAINER_USER=root,
  // _CONTAINER_USER_HOME=/root, and sanitized option GREETING=hi.
  assert_eq!(configs[0].user.as_ref().map(|u| (u.name.as_str(), u.uid, u.gid)), Some(("root", 0, 0)));
  assert_eq!(configs[0].env.get("_REMOTE_USER").map(String::as_str), Some("dev"));
  assert_eq!(configs[0].env.get("_REMOTE_USER_HOME").map(String::as_str), Some("/home/dev"));
  assert_eq!(configs[0].env.get("_CONTAINER_USER").map(String::as_str), Some("root"));
  assert_eq!(configs[0].env.get("_CONTAINER_USER_HOME").map(String::as_str), Some("/root"));
  assert_eq!(configs[0].env.get("GREETING").map(String::as_str), Some("hi"));
  assert!(
    configs[0].copy_in.iter().any(|c| c.path == "/tmp/needy-feature"),
    "feature build step must stage archive via copy_in: {:?}",
    configs[0].copy_in
  );

  // Step 1 (`node`): stacked on step 0, exports VERSION=22 and default NODE_GYP_DEPENDENCIES=true.
  assert_eq!(configs[1].layers.len(), 1);
  assert_eq!(configs[1].env.get("VERSION").map(String::as_str), Some("22"));
  assert_eq!(configs[1].env.get("NODE_GYP_DEPENDENCIES").map(String::as_str), Some("true"));
  assert_eq!(configs[1].env.get("_REMOTE_USER").map(String::as_str), Some("dev"));

  // RunInfo returns resolved canonical_ref -> digest for both features.
  assert_eq!(created.resolved_features.get("ghcr.io/devcontainers/features/common").map(String::as_str), Some(common_digest.as_str()));
  assert_eq!(created.resolved_features.get("ghcr.io/devcontainers/features/node").map(String::as_str), Some(node_digest.as_str()));

  // Final actor receives merged containerEnv and PATH from both features in order.
  let guest_cfg = Guest::config(daemon, &created.id).await.unwrap();
  assert_eq!(guest_cfg.env.get("COMMON_DIR").map(String::as_str), Some("/opt/common"));
  assert_eq!(guest_cfg.env.get("NODE_HOME").map(String::as_str), Some("/usr/local/share/nvm"));
  assert_eq!(guest_cfg.image_path, format!("/usr/local/share/nvm/bin:/opt/common/bin:{}", needy_core::image::SYSTEM_PATH));

  Runs::stop(daemon, &created.id, needy_core::types::Principal::Operator, needy_api_server::control::StopMode::Now).await.unwrap();
  stop.cancel();
  responder.await.unwrap();
}

#[tokio::test]
async fn second_build_with_pinned_features_makes_zero_registry_requests_and_zero_rebuilds() {
  use needy_api_server::control::{ImageBuild, Images};
  use needy_core_actor::image::feature::FeatureStore;

  let git_feature =
    make_feature_artifact("devcontainers/features/git", "1", r#"{"id":"git","version":"1.3.0"}"#, "#!/bin/sh\necho installing git\n");
  let git_digest = git_feature.manifest_digest.to_string();
  let (base_url, requests) = spawn_feature_registry(vec![git_feature]).await;

  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let feature_store = FeatureStore::new(&daemon.config.cache_dir).unwrap().without_proxy().unwrap().with_base_url(&base_url);
  daemon.images.set_feature_store(feature_store);

  let started_log = started.dir.path().join("started.log");
  fake_image(&started.dir.path().join("image"), &session_driver_script(&started_log));

  let stop = tokio_util::sync::CancellationToken::new();
  let behavior = Arc::new(std::sync::Mutex::new(BuildBehavior::default()));
  let (sessions, _configs, responder) = spawn_build_responder(started_log, behavior, stop.clone());

  let image_yaml = "root: needy-actor:latest\nlayers:\n  - feature: ghcr.io/devcontainers/features/git:1\n";

  let first = tokio::time::timeout(
    Duration::from_secs(20),
    Images::build(
      daemon,
      ImageBuild {
        image_yaml: image_yaml.into(),
        driver: "linux-firecracker".into(),
        user: None,
        commit: None,
        pinned_features: Default::default(),
      },
    ),
  )
  .await
  .expect("first feature build timed out")
  .unwrap();

  assert_eq!(sessions.load(Ordering::SeqCst), 1);
  assert_eq!(first.resolved_features.get("ghcr.io/devcontainers/features/git").map(String::as_str), Some(git_digest.as_str()));

  // Reset HTTP request counter and rebuild with pinned_features from the first build.
  requests.store(0, Ordering::SeqCst);
  let second = tokio::time::timeout(
    Duration::from_secs(20),
    Images::build(
      daemon,
      ImageBuild {
        image_yaml: image_yaml.into(),
        driver: "linux-firecracker".into(),
        user: None,
        commit: None,
        pinned_features: first.resolved_features.clone(),
      },
    ),
  )
  .await
  .expect("second pinned feature build timed out")
  .unwrap();

  assert_eq!(requests.load(Ordering::SeqCst), 0, "pinned build must make zero registry HTTP requests");
  assert_eq!(sessions.load(Ordering::SeqCst), 1, "pinned build must reuse cached layer with zero rebuilds");
  assert_eq!(second.snapshot, first.snapshot);
  assert!(second.steps[0].cached);

  stop.cancel();
  responder.await.unwrap();
}

#[tokio::test]
async fn privileged_feature_is_refused_before_starting_any_actor() {
  use needy_api_server::control::{ImageBuild, Images};
  use needy_core_actor::image::feature::FeatureStore;

  let priv_feature = make_feature_artifact(
    "example/features/docker-in-docker",
    "1",
    r#"{"id":"docker-in-docker","version":"1.0.0","privileged":true}"#,
    "#!/bin/sh\nexit 0\n",
  );
  let (base_url, _requests) = spawn_feature_registry(vec![priv_feature]).await;

  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let feature_store = FeatureStore::new(&daemon.config.cache_dir).unwrap().without_proxy().unwrap().with_base_url(&base_url);
  daemon.images.set_feature_store(feature_store);

  let started_log = started.dir.path().join("started.log");
  fake_image(&started.dir.path().join("image"), &session_driver_script(&started_log));

  let stop = tokio_util::sync::CancellationToken::new();
  let behavior = Arc::new(std::sync::Mutex::new(BuildBehavior::default()));
  let (sessions, _configs, responder) = spawn_build_responder(started_log.clone(), behavior, stop.clone());

  let image_yaml = "root: needy-actor:latest\nlayers:\n  - feature: ghcr.io/example/features/docker-in-docker:1\n";

  let err = Images::build(
    daemon,
    ImageBuild {
      image_yaml: image_yaml.into(),
      driver: "linux-firecracker".into(),
      user: None,
      commit: None,
      pinned_features: Default::default(),
    },
  )
  .await
  .expect_err("privileged feature must be refused");

  let msg = err.to_string();
  assert!(msg.contains("privileged"), "error must name the refused property: {msg}");
  assert!(msg.contains("docker-in-docker"), "error must name the feature: {msg}");
  assert_eq!(sessions.load(Ordering::SeqCst), 0, "no actor/build session may start for a refused feature");
  assert!(!started_log.exists(), "no VM process may be spawned for a refused feature");

  stop.cancel();
  responder.await.unwrap();
}

#[tokio::test]
async fn guest_config_receives_merged_hooks_for_runs_and_empty_hooks_for_build_and_edit_sessions() {
  use needy_api_server::control::{Guest, ImageEditStart, Images, Runs, StopMode};
  use needy_core_actor::image::feature::FeatureStore;

  let hook_feature = make_feature_artifact(
    "example/features/hooks",
    "1",
    r#"{"id":"hooks","version":"1.0.0","postCreateCommand":"feature-create","postStartCommand":["feature-start","--ok"]}"#,
    "#!/bin/sh\nexit 0\n",
  );
  let (base_url, _requests) = spawn_feature_registry(vec![hook_feature]).await;

  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let feature_store = FeatureStore::new(&daemon.config.cache_dir).unwrap().without_proxy().unwrap().with_base_url(&base_url);
  daemon.images.set_feature_store(feature_store);

  let started_log = started.dir.path().join("started.log");
  fake_image(&started.dir.path().join("image"), &session_driver_script(&started_log));

  let stop = tokio_util::sync::CancellationToken::new();
  let behavior = Arc::new(std::sync::Mutex::new(BuildBehavior::default()));
  let (_sessions, configs, responder) = spawn_build_responder(started_log.clone(), behavior, stop.clone());

  let image_yaml = r#"
root: needy-actor:latest
layers:
  - feature: ghcr.io/example/features/hooks:1
hooks:
  start:
    - image-start
  stop:
    - image-stop
"#;
  let profile = format!(
    "actor:\n  driver: linux-firecracker\n  image:\n{}\nuser: {{ name: agent, uid: 1000, gid: 1000 }}\nexecutors:\n  here: {{ driver: native }}\n",
    image_yaml.lines().map(|l| format!("    {l}")).collect::<Vec<_>>().join("\n")
  );

  // Creating a normal run builds the feature (a build session) and prepares the run.
  let created = tokio::time::timeout(Duration::from_secs(20), Runs::create(daemon, new_run(&profile, started.dir.path())))
    .await
    .expect("run create with hooks timed out")
    .unwrap();

  // Build session must have received empty hooks.
  {
    let seen = configs.lock().unwrap();
    assert!(!seen.is_empty(), "expected at least one build session GetConfig");
    for build_cfg in seen.iter() {
      assert!(build_cfg.start_hooks.is_empty(), "build session must not run start_hooks: {:?}", build_cfg.start_hooks);
      assert!(build_cfg.stop_hooks.is_empty(), "build session must not run stop_hooks: {:?}", build_cfg.stop_hooks);
    }
  }

  // Normal run's Guest::config receives feature hooks first, then image hooks.
  let run_cfg = Guest::config(daemon, &created.id).await.unwrap();
  assert_eq!(run_cfg.start_hooks, ["feature-create", "'feature-start' '--ok'", "image-start"]);
  assert_eq!(run_cfg.stop_hooks, ["image-stop"]);

  Runs::stop(daemon, &created.id, needy_core::types::Principal::Operator, StopMode::Now).await.unwrap();

  // Edit session on the same image must receive empty hooks.
  configs.lock().unwrap().clear();
  let edit = tokio::time::timeout(
    Duration::from_secs(20),
    Images::start_edit(
      daemon,
      ImageEditStart {
        image_name: "hooks-img".into(),
        image_yaml: image_yaml.into(),
        driver: "linux-firecracker".into(),
        user: None,
        commit: None,
        tty: None,
        wait_for_attach: false,
        pinned_features: Default::default(),
      },
    ),
  )
  .await
  .expect("start_edit timed out")
  .unwrap();

  let _ = tokio::time::timeout(Duration::from_secs(20), Images::finish_edit(daemon, &edit.session_id, false))
    .await
    .expect("finish_edit timed out")
    .unwrap();

  {
    let seen = configs.lock().unwrap();
    assert!(!seen.is_empty(), "expected edit session GetConfig");
    for edit_cfg in seen.iter() {
      assert!(edit_cfg.start_hooks.is_empty(), "edit session must not run start_hooks: {:?}", edit_cfg.start_hooks);
      assert!(edit_cfg.stop_hooks.is_empty(), "edit session must not run stop_hooks: {:?}", edit_cfg.stop_hooks);
    }
  }

  stop.cancel();
  responder.await.unwrap();
}

#[tokio::test]
async fn feature_declared_sync_mapping_seeds_mounts_merges_and_appears_in_report() {
  use needy_core::event::{Event, Recorded};
  use needy_core::report::{self, Format, Level};
  use needy_core_actor::image::feature::FeatureStore;

  let opencode = make_feature_artifact(
    "example/features/opencode",
    "1",
    r#"{
      "id": "opencode",
      "version": "1.0.0",
      "customizations": {
        "needy": {
          "files": {
            "opencode": {
              "path": "~/.opencode",
              "filter": ["- cache/"],
              "on-conflict": { "history.jsonl": "lines" }
            },
            "opencode-config": {
              "path": "~/.config/opencode"
            }
          }
        }
      }
    }"#,
    "#!/bin/sh\necho installing opencode\n",
  );
  let (base_url, _requests) = spawn_feature_registry(vec![opencode]).await;

  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let dir = started.dir.path();
  let data = dir.join("data");
  let feature_store = FeatureStore::new(&daemon.config.cache_dir).unwrap().without_proxy().unwrap().with_base_url(&base_url);
  daemon.images.set_feature_store(feature_store);

  let started_log = dir.join("started.log");
  fake_image(&dir.join("image"), &session_driver_script(&started_log));

  let stop = tokio_util::sync::CancellationToken::new();
  let behavior = Arc::new(std::sync::Mutex::new(BuildBehavior::default()));
  let (_sessions, _configs, responder) = spawn_build_responder(started_log, behavior, stop.clone());

  // Pre-seed one file and one filtered cache file in `<data_dir>/files/opencode`.
  let store = data.join("files/opencode");
  std::fs::create_dir_all(store.join("cache")).unwrap();
  std::fs::write(store.join("auth.json"), "initial-token").unwrap();
  std::fs::write(store.join("cache/tmp.bin"), "ignored").unwrap();

  let profile = r#"
actor:
  driver: linux-firecracker
  image:
    root: needy-actor:latest
    layers:
      - feature: ghcr.io/example/features/opencode:1
user: { name: dev, uid: 1001, gid: 1001 }
executors:
  here: { driver: native }
commands:
  - { match: "**", to: here }
"#;

  let created = tokio::time::timeout(Duration::from_secs(20), Runs::create(daemon, new_run(profile, dir)))
    .await
    .expect("run create with feature sync mapping timed out")
    .unwrap();

  // Per-run copy is seeded from `<data_dir>/files/opencode` through the filter,
  // `<data_dir>/files/opencode-config` is auto-created when missing, and
  // `Guest::config` includes both pass-through mounts.
  let auto_store = data.join("files/opencode-config");
  assert!(auto_store.is_dir(), "missing feature store under data_dir must be auto-created");
  let copy = data.join("sync/runs").join(&created.id).join("opencode/copy");
  assert_eq!(std::fs::read_to_string(copy.join("auth.json")).unwrap(), "initial-token");
  assert!(!copy.join("cache").exists(), "feature filter must exclude cache/ from per-run copy");

  let guest_cfg = Guest::config(daemon, &created.id).await.unwrap();
  let mount =
    guest_cfg.mounts.iter().find(|m| m.target == "/home/dev/.opencode").expect("Guest::config must include feature-declared sync mount");
  assert_eq!(mount.executor, crate::sync::EXECUTOR);
  assert_eq!(mount.source, copy.to_str().unwrap());
  assert!(mount.writable);
  assert!(
    guest_cfg.mounts.iter().any(|m| m.target == "/home/dev/.config/opencode"),
    "Guest::config must include auto-created feature sync mount"
  );

  // Simulate the actor refreshing auth.json and writing a session file.
  std::fs::write(copy.join("auth.json"), "refreshed-token").unwrap();
  std::fs::create_dir_all(copy.join("sessions")).unwrap();
  std::fs::write(copy.join("sessions/s1.jsonl"), "hello\n").unwrap();

  Runs::stop(daemon, &created.id, needy_core::types::Principal::Operator, needy_api_server::control::StopMode::Now).await.unwrap();

  // Changes are merged back into `<data_dir>/files/opencode`, and the per-run copy is removed.
  assert_eq!(std::fs::read_to_string(store.join("auth.json")).unwrap(), "refreshed-token");
  assert_eq!(std::fs::read_to_string(store.join("sessions/s1.jsonl")).unwrap(), "hello\n");
  assert!(!data.join("sync/runs").join(&created.id).exists());

  // `run report` shows the merged feature sync mapping.
  let trace = needy_trace::sink::sqlite::load(&dir.join("state/trace.db"), &created.id).await.unwrap();
  let events: Vec<Recorded> = trace
    .events
    .iter()
    .map(|e| Recorded {
      time_ms: e.time_ms,
      event: Event::decode(&e.kind, e.version, e.payload.get()).unwrap(),
    })
    .collect();
  let summary: serde_json::Value = serde_json::from_str(&report::render(&created.id, &events, Level::Summary, Format::Json)).unwrap();
  assert_eq!(summary["synced"][0]["name"], "opencode");
  assert_eq!(summary["synced"][0]["store"], store.display().to_string());
  assert_eq!(summary["synced"][0]["merge"], "merged");

  stop.cancel();
  responder.await.unwrap();
}

#[tokio::test]
async fn profile_explicit_file_mapping_overrides_feature_declared_sync_mapping() {
  use needy_core_actor::image::feature::FeatureStore;

  let opencode = make_feature_artifact(
    "example/features/opencode",
    "1",
    r#"{
      "id": "opencode",
      "version": "1.0.0",
      "customizations": {
        "needy": {
          "files": {
            "opencode": {
              "path": "~/.opencode"
            }
          }
        }
      }
    }"#,
    "#!/bin/sh\necho installing opencode\n",
  );
  let (base_url, _requests) = spawn_feature_registry(vec![opencode]).await;

  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let dir = started.dir.path();
  let data = dir.join("data");
  let feature_store = FeatureStore::new(&daemon.config.cache_dir).unwrap().without_proxy().unwrap().with_base_url(&base_url);
  daemon.images.set_feature_store(feature_store);

  let started_log = dir.join("started.log");
  fake_image(&dir.join("image"), &session_driver_script(&started_log));

  let stop = tokio_util::sync::CancellationToken::new();
  let behavior = Arc::new(std::sync::Mutex::new(BuildBehavior::default()));
  let (_sessions, _configs, responder) = spawn_build_responder(started_log, behavior, stop.clone());

  // Profile explicitly maps the same actor path (`~/.opencode`) to a custom store
  // under a different mapping name (`work_opencode`).
  let custom_store = dir.join("work-opencode");
  std::fs::create_dir_all(&custom_store).unwrap();
  std::fs::write(custom_store.join("account.txt"), "work").unwrap();

  let profile = format!(
    r#"
actor:
  driver: linux-firecracker
  image:
    root: needy-actor:latest
    layers:
      - feature: ghcr.io/example/features/opencode:1
user: {{ name: dev, uid: 1001, gid: 1001 }}
files:
  work_opencode:
    mode: sync
    from: "host:{}"
    to: "actor:~/.opencode"
executors:
  here: {{ driver: native }}
commands:
  - {{ match: "**", to: here }}
"#,
    custom_store.display()
  );

  let created = tokio::time::timeout(Duration::from_secs(20), Runs::create(daemon, new_run(&profile, dir)))
    .await
    .expect("run create with overridden feature sync mapping timed out")
    .unwrap();

  // Only the profile's explicit mapping mounts `/home/dev/.opencode`, backed by `work_opencode`.
  let guest_cfg = Guest::config(daemon, &created.id).await.unwrap();
  let opencode_mounts: Vec<_> = guest_cfg.mounts.iter().filter(|m| m.target == "/home/dev/.opencode").collect();
  assert_eq!(opencode_mounts.len(), 1, "profile override must suppress feature default mount");

  let custom_copy = data.join("sync/runs").join(&created.id).join("work_opencode/copy");
  assert_eq!(opencode_mounts[0].source, custom_copy.to_str().unwrap());
  assert_eq!(std::fs::read_to_string(custom_copy.join("account.txt")).unwrap(), "work");
  assert!(!data.join("files/opencode").exists(), "overridden feature default store must not be created");

  Runs::stop(daemon, &created.id, needy_core::types::Principal::Operator, needy_api_server::control::StopMode::Now).await.unwrap();
  stop.cancel();
  responder.await.unwrap();
}

// ROOT CAUSE:
//
// If a Dev Container feature's `install.sh` uses a bash shebang
// (`#!/usr/bin/env bash` or `#!/bin/bash`, as official features like
// `ghcr.io/devcontainers/features/node:1` do) and bash syntax such as
// parameter substitution, running `sh ./install.sh` failed under Debian's
// dash `/bin/sh` with `Bad substitution` (exit 2) because `sh` ignored the
// script's `#!` interpreter.
//
// Before the fix, `feature_install_script` did `chmod +x ./install.sh` and
// then ran `sh ./install.sh || status=$?`.
// The fix executes `./install.sh || status=$?` directly so the kernel honors
// the feature script's shebang (and POSIX `sh` still falls back to `/bin/sh`
// when no shebang is present).
#[test]
fn feature_install_script_executes_install_sh_directly_to_honor_shebang() {
  let user = needy_config::profile::User {
    name: "agent".into(),
    uid: 1000,
    gid: 1000,
  };
  let script = super::feature_install_script(&user, &std::collections::BTreeMap::new());
  assert!(
    script.contains("\n./install.sh || status=$?\n"),
    "wrapper must execute `./install.sh` directly so its #! shebang is honored:\n{script}"
  );
  assert!(!script.contains("sh ./install.sh"), "wrapper must not force `/bin/sh` over a feature's shebang:\n{script}");
}

// ROOT CAUSE:
//
// If a failing build step (such as a Dev Container feature running `curl`)
// emitted carriage-return (`\r`) progress output or very long console lines,
// `format_build_failure` included the raw lines unbounded in the gRPC
// `FAILED_PRECONDITION` status message. After percent-encoding into the HTTP/2
// `grpc-message` trailer, the header exceeded h2's 16 KiB
// `MAX_HEADER_LIST_SIZE`, causing the client to fail with an opaque
// `h2 protocol error: http2 error` instead of the build step's real error.
//
// Before the fix, `format_build_failure` neither collapsed `\r` progress
// updates nor capped the byte length of the console tail.
// The fix keeps only the final `\r` segment per line and caps the console tail
// to `FAILURE_LOG_MAX_BYTES`.
#[test]
fn format_build_failure_collapses_carriage_returns_and_bounds_message_size() {
  let progress = format!("{}final progress line\nactual error\n", "### 12.3%\r".repeat(3000));
  let msg = super::format_build_failure("feature: example/node:1", "exited with code 1", &progress);
  assert!(msg.contains("final progress line"), "expected final progress segment: {msg}");
  assert!(msg.contains("actual error"), "expected error line: {msg}");
  assert!(!msg.contains('\r'), "carriage returns must be collapsed: {msg}");
  assert!(msg.len() <= super::FAILURE_LOG_MAX_BYTES + 256, "message too large for grpc-message trailer: {} bytes", msg.len());

  let huge = format!("{}\nlast-line\n", "x".repeat(20_000));
  let bounded = super::format_build_failure("feature: example/node:1", "exited with code 1", &huge);
  assert!(bounded.ends_with("last-line"), "tail must be preserved: {bounded}");
  assert!(bounded.len() <= super::FAILURE_LOG_MAX_BYTES + 256, "huge console line must be bounded: {} bytes", bounded.len());
}

#[tokio::test]
async fn resolve_features_resolves_builtin_agent_and_custom_root_sshfs_without_network() {
  use needy_core::image::{BUILTIN_SSHFS, Input};
  use needy_core_actor::image::feature::FeatureStore;

  let cache = tempfile::tempdir().unwrap();
  // Point at an unreachable loopback port so any unexpected HTTP request fails immediately.
  let store = FeatureStore::new(cache.path()).unwrap().without_proxy().unwrap().with_base_url("http://127.0.0.1:1");

  let def: needy_config::image::Definition = needy_config::image::load(&needy_config::Source::text(
    r#"
root: debian:bookworm-slim
layers:
  - feature: builtin:codex
  - feature: builtin:pi
"#,
    PathBuf::new(),
  ))
  .unwrap();

  let resolved = super::resolve_features(&store, &def, &std::collections::BTreeMap::new()).await.unwrap();
  assert!(resolved.by_ref.contains_key(BUILTIN_SSHFS), "custom root must resolve builtin:sshfs");
  assert!(resolved.by_ref.contains_key("builtin:codex"));
  assert!(resolved.by_ref.contains_key("builtin:pi"));
  assert!(resolved.resolved_digests.contains_key("builtin:codex"));

  let root = needy_core::image::Digest::of(b"debian:bookworm-slim");
  let user = needy_config::profile::User::default();
  let plan = needy_core::image::plan(&def, &root, &user, None, &resolved.by_ref).unwrap();
  assert_eq!(plan.steps.len(), 3);
  assert!(matches!(&plan.steps[0].input, Input::Feature { reference, .. } if reference == BUILTIN_SSHFS));
  assert!(matches!(&plan.steps[1].input, Input::Feature { reference, .. } if reference == "builtin:codex"));
  assert!(matches!(&plan.steps[2].input, Input::Feature { reference, .. } if reference == "builtin:pi"));

  let sync_paths: Vec<&str> = plan.sync_files.iter().map(|s| s.path.as_str()).collect();
  assert!(sync_paths.contains(&"/home/agent/.codex"), "{sync_paths:?}");
  assert!(sync_paths.contains(&"/home/agent/.pi/agent"), "{sync_paths:?}");
}

#[test]
fn migrated_example_agent_profiles_load_validate_and_use_builtin_agent_features() {
  let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
  let cases = [
    ("examples/agents/codex.yaml", vec!["builtin:codex"]),
    ("examples/agents/pi.yaml", vec!["builtin:pi"]),
    ("examples/agents/opencode.yaml", vec!["builtin:opencode"]),
    (
      "examples/agents/herdr.yaml",
      vec![
        "builtin:codex",
        "builtin:pi",
        "builtin:opencode",
        "builtin:herdr",
      ],
    ),
  ];

  for (rel, expected_features) in cases {
    let path = root.join(rel);
    let profile =
      needy_config::profile::load(&needy_config::Source::file(path.clone())).unwrap_or_else(|e| panic!("failed to load {rel}: {e}"));
    let findings = needy_config::validate::profile(&profile);
    assert!(findings.is_empty(), "validation findings for {rel}: {findings:?}");
    let Some(needy_config::image::Image::Definition(def)) = &profile.actor.image else {
      panic!("{rel} must use an inline image definition");
    };
    let actual: Vec<&str> = def
      .layers
      .iter()
      .filter_map(|l| match l {
        needy_config::image::Layer::Feature { reference, .. } => Some(reference.as_str()),
        needy_config::image::Layer::Run(_) => None,
      })
      .collect();
    assert_eq!(actual, expected_features, "unexpected feature layers in {rel}");
  }
}

#[tokio::test]
async fn image_log_returns_empty_before_edits_and_records_commits_with_commands_base_and_active() {
  use needy_api_server::control::{ImageLog, Images as _};
  use needy_core_actor::image::store::Command as StoreCommand;

  let started = testing::daemon().await;
  let daemon = &started.daemon;

  // Before any edits, `Images::log` returns an empty history.
  let empty = daemon
    .log(ImageLog {
      image_name: "demo-image".into(),
      current_layer: None,
    })
    .await
    .unwrap();
  assert!(empty.commits.is_empty());

  // Record two commits in the store (simulating two edit sessions).
  let base1 = needy_core::image::Digest::of(b"base-plan-1");
  let base2 = needy_core::image::Digest::of(b"base-plan-2");
  let layer1 = daemon
    .images
    .store
    .commit(
      "demo-image",
      None,
      Some(&base1),
      std::io::Cursor::new(tar_with_file("first.txt", "one\n")),
      vec![StoreCommand::new(
        vec!["apt-get".into(), "update".into()],
        "/",
      )],
    )
    .unwrap();
  let layer2 = daemon
    .images
    .store
    .commit(
      "demo-image",
      Some(&layer1),
      Some(&base2),
      std::io::Cursor::new(tar_with_file("second.txt", "two\n")),
      vec![StoreCommand::new(
        vec!["pip".into(), "install".into(), "ruff".into()],
        "/opt",
      )],
    )
    .unwrap();

  let logged = daemon
    .log(ImageLog {
      image_name: "demo-image".into(),
      current_layer: Some(layer2.to_string()),
    })
    .await
    .unwrap();
  assert_eq!(logged.commits.len(), 2);

  assert_eq!(logged.commits[0].layer, layer1.to_string());
  assert_eq!(logged.commits[0].parent, None);
  assert_eq!(logged.commits[0].base.as_deref(), Some(base1.to_string().as_str()));
  assert!(!logged.commits[0].active);
  assert_eq!(logged.commits[0].commands.len(), 1);
  assert_eq!(logged.commits[0].commands[0].argv, ["apt-get", "update"]);
  assert_eq!(logged.commits[0].commands[0].cwd, "/");

  assert_eq!(logged.commits[1].layer, layer2.to_string());
  assert_eq!(logged.commits[1].parent.as_deref(), Some(layer1.to_string().as_str()));
  assert_eq!(logged.commits[1].base.as_deref(), Some(base2.to_string().as_str()));
  assert!(logged.commits[1].active);
  assert_eq!(logged.commits[1].commands.len(), 1);
  assert_eq!(logged.commits[1].commands[0].argv, ["pip", "install", "ruff"]);
  assert_eq!(logged.commits[1].commands[0].cwd, "/opt");
}

#[tokio::test]
async fn image_export_streams_valid_oci_docker_archive() {
  use futures_util::StreamExt as _;
  use needy_api_server::control::{ImageExport, Images as _};

  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let started_log = started.dir.path().join("started.log");
  fake_image(&started.dir.path().join("image"), &session_driver_script(&started_log));

  let stop = tokio_util::sync::CancellationToken::new();
  let behavior = Arc::new(std::sync::Mutex::new(BuildBehavior::default()));
  let (_sessions, _configs, responder) = spawn_build_responder(started_log.clone(), behavior, stop.clone());

  let image_yaml = "layers:\n  - run: echo export-layer\n".to_string();
  let mut stream = tokio::time::timeout(
    Duration::from_secs(20),
    daemon.export(ImageExport {
      image_yaml,
      driver: "linux-firecracker".into(),
      user: None,
      commit: None,
      pinned_features: Default::default(),
      tag: Some("example.com/exported:1".into()),
    }),
  )
  .await
  .expect("image export timed out")
  .unwrap();

  let mut bytes = Vec::new();
  while let Some(chunk) = stream.next().await {
    bytes.extend_from_slice(&chunk.unwrap());
  }
  stop.cancel();
  responder.abort();

  assert!(!bytes.is_empty(), "exported archive must not be empty");
  let mut archive = tar::Archive::new(std::io::Cursor::new(&bytes));
  let mut names = std::collections::BTreeSet::new();
  let mut manifest_json = String::new();
  for entry in archive.entries().unwrap() {
    let mut entry = entry.unwrap();
    let path = entry.path().unwrap().to_string_lossy().into_owned();
    if path == "manifest.json" {
      std::io::Read::read_to_string(&mut entry, &mut manifest_json).unwrap();
    }
    names.insert(path);
  }
  assert!(names.contains("oci-layout"), "missing oci-layout: {names:?}");
  assert!(names.contains("index.json"), "missing index.json: {names:?}");
  assert!(names.contains("manifest.json"), "missing manifest.json: {names:?}");
  assert!(manifest_json.contains("example.com/exported:1"), "tag missing from manifest.json: {manifest_json}");
}
