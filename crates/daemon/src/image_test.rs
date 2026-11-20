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
    root: example.com/root:1
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
    root: example.com/root:1
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
    root: example.com/root:1
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
root: example.com/root:1
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
  let empty_image_yaml = "root: example.com/root:1\n";

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

  // Roll back 1 step from commit2 -> commit1.
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
}

#[tokio::test]
async fn run_create_with_lockfile_commit_stacks_commit_layer_and_reports_rebase() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;

  let profile = r#"
actor:
  driver: linux-firecracker
  image:
    root: example.com/root:1
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  here: { driver: native }
"#;

  // Store a commit layer in the daemon's layer store.
  let commit_layer =
    daemon.images.store.commit("test-img", None, std::io::Cursor::new(tar_with_file("hand.txt", "edited\n")), Vec::new()).unwrap();
  let root = crate::image::root_digest(daemon).await.unwrap();
  let def = needy_config::image::Definition {
    root: Some("example.com/root:1".into()),
    ..Default::default()
  };
  let expected_top = needy_core::image::plan(&def, &root, &needy_config::profile::User::default(), None).top().clone();
  let commit_digest = needy_core::image::Digest::parse(&commit_layer.to_string()).unwrap();

  // Matching base -> commit_rebased == false, commit layer stacked on top.
  let mut req = new_run(profile, started.dir.path());
  req.image_commit = Some(needy_core::image::Commit {
    layer: commit_digest.clone(),
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
    layer: commit_digest,
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
