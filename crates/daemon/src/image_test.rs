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
    root: example.com/root:1
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  here: { driver: native }
"#;

  // Store a commit layer in the daemon's layer store.
  let commit_layer =
    daemon.images.store.commit("test-img", None, None, std::io::Cursor::new(tar_with_file("hand.txt", "edited\n")), Vec::new()).unwrap();
  let root = crate::image::root_digest(daemon).await.unwrap();
  let def = needy_config::image::Definition {
    root: Some("example.com/root:1".into()),
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
