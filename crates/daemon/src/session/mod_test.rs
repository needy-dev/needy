use std::collections::BTreeMap;

use needy_api_server::control::{ControlError, Sessions};
use needy_core::intercept::ExecContext;
use needy_core::types::SessionState;

use crate::testing::{self, fake_image, new_actor_session, new_session};

const PROFILE: &str = r#"
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  here: { runtime: native }
commands:
  - { match: "**", to: here }
"#;

#[tokio::test]
async fn a_created_session_runs_on_its_executor_until_stopped() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let created = Sessions::create(daemon, new_session(PROFILE, started.dir.path())).await.unwrap();
  assert_eq!(created.state, SessionState::Running);
  assert_eq!(created.name, created.id, "an unnamed session is named after its ID");
  assert_eq!(created.profile, "test");
  let executors: Vec<_> = created.executors.iter().map(|e| (e.name.as_str(), e.runtime.as_str())).collect();
  assert_eq!(executors, [("here", "native")]);
  assert!(started.dir.path().join("state/sessions").join(&created.id).join("profile.yaml").is_file());

  Sessions::stop(daemon, &created.id).await.unwrap();
  let stopped = Sessions::get(daemon, &created.id).await.unwrap();
  assert_eq!(stopped.state, SessionState::Stopped);
  let listed = Sessions::list(daemon).await.unwrap();
  assert_eq!(listed.len(), 1);
  assert_eq!(listed[0].state, SessionState::Stopped);
  assert!(matches!(Sessions::stop(daemon, &created.id).await, Err(ControlError::NotFound(_))), "a stopped session is not stopped again");
}

#[tokio::test]
async fn a_session_whose_profile_is_invalid_is_listed_as_failed() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let profile = "commands:\n  - { match: \"**\", to: nowhere }\n";
  let Err(ControlError::Failed(message)) = Sessions::create(daemon, new_session(profile, started.dir.path())).await else {
    panic!("an invalid profile must fail the create");
  };
  assert!(message.contains("invalid profile"), "{message}");
  let listed = Sessions::list(daemon).await.unwrap();
  assert_eq!(listed.len(), 1);
  assert_eq!(listed[0].state, SessionState::Failed);
}

#[tokio::test]
async fn sessions_are_found_by_id_unique_prefix_or_name() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let mut request = new_session(PROFILE, started.dir.path());
  request.name = "named".into();
  let created = Sessions::create(daemon, request).await.unwrap();
  assert_eq!(Sessions::get(daemon, &created.id).await.unwrap().id, created.id);
  assert_eq!(Sessions::get(daemon, &created.id[..4]).await.unwrap().id, created.id);
  assert_eq!(Sessions::get(daemon, "named").await.unwrap().id, created.id);
  assert!(matches!(Sessions::get(daemon, "zzzz").await, Err(ControlError::NotFound(_))));
}

// ROOT CAUSE:
//
// If a lookup was given an empty session ID, every session matched it as a
// prefix, so with one session `get`, `stop` and `Decide` acted on that
// session.
//
// Before the fix, `needy session stop ""` stopped the only session.
// The fix keeps an empty ID naming no session.
#[tokio::test]
async fn an_empty_id_names_no_session() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let created = Sessions::create(daemon, new_session(PROFILE, started.dir.path())).await.unwrap();
  assert!(matches!(Sessions::get(daemon, "").await, Err(ControlError::NotFound(_))));
  assert!(matches!(Sessions::stop(daemon, "").await, Err(ControlError::NotFound(_))));
  let ctx = ExecContext {
    argv: vec!["true".into()],
    ..ExecContext::default()
  };
  assert!(matches!(Sessions::decide(daemon, "", ctx).await, Err(ControlError::NotFound(_))));
  assert_eq!(Sessions::get(daemon, &created.id).await.unwrap().state, SessionState::Running);
}

#[tokio::test]
async fn a_prefix_of_several_sessions_is_ambiguous() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  // Seventeen IDs of hex digits share a first digit at least twice.
  let mut by_first: BTreeMap<char, Vec<String>> = BTreeMap::new();
  for _ in 0..17 {
    let created = Sessions::create(daemon, new_session(PROFILE, started.dir.path())).await.unwrap();
    by_first.entry(created.id.chars().next().unwrap()).or_default().push(created.id);
  }
  let (first, ids) = by_first.iter().find(|(_, ids)| ids.len() > 1).unwrap();
  let Err(ControlError::Invalid(message)) = Sessions::stop(daemon, &first.to_string()).await else {
    panic!("an ambiguous prefix must be refused");
  };
  assert!(message.contains("ambiguous"), "{message}");
  for id in ids {
    assert_eq!(Sessions::get(daemon, id).await.unwrap().state, SessionState::Running);
  }
}

// ROOT CAUSE:
//
// If a session's create failed after it had acquired some of its executors,
// those instances stayed in the pool, because the rollback stopped the
// session's tasks and recorded the failure but released nothing.
//
// Before the fix, a profile whose second executor could not be created left
// the first one's instance pooled (for Docker, a running container).
// The fix keeps a failed create holding nothing: the rollback releases
// every instance the create acquired.
#[tokio::test]
async fn a_failed_create_releases_the_executors_it_acquired() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let remote = testing::Executor::start().await;
  // `b` is acquired after `a` and fails: its token's variable is not set.
  let profile = format!(
    r#"
user: {{ name: agent, uid: 1000, gid: 1000 }}
executors:
  a: {{ runtime: native }}
  b: {{ runtime: remote, remote: {{ url: "{}", token_env: NEEDY_TEST_UNSET_TOKEN }} }}
commands:
  - {{ match: "**", to: a }}
"#,
    remote.endpoint
  );
  let Err(ControlError::Failed(message)) = Sessions::create(daemon, new_session(&profile, started.dir.path())).await else {
    panic!("a create whose executor fails must fail");
  };
  assert!(message.contains("executor b"), "{message}");

  assert!(daemon.pool.instances().await.is_empty(), "the failed create still holds an executor");
  let listed = Sessions::list(daemon).await.unwrap();
  assert_eq!(listed.len(), 1);
  assert_eq!(listed[0].state, SessionState::Failed);
  let released: Vec<&str> = listed[0].released.iter().map(|e| e.name.as_str()).collect();
  assert_eq!(released, ["a"], "a failed session lists the executors it gave back");
  assert!(listed[0].failure.as_deref().is_some_and(|f| f.contains("executor b")), "{:?}", listed[0].failure);
}

#[tokio::test]
async fn a_create_whose_actor_cannot_start_releases_its_executors_and_sockets() {
  use std::os::unix::fs::PermissionsExt;
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let image = started.dir.path().join("image");
  fake_image(&image, "exit 0");
  // Firecracker cannot be started once the guest listener and the gateway
  // listen.
  std::fs::set_permissions(image.join("firecracker"), std::fs::Permissions::from_mode(0o644)).unwrap();

  let Err(ControlError::Failed(message)) = Sessions::create(daemon, new_actor_session(PROFILE, started.dir.path())).await else {
    panic!("a create whose actor cannot start must fail");
  };
  assert!(message.contains("firecracker"), "{message}");

  assert!(daemon.pool.instances().await.is_empty(), "the failed create still holds an executor");
  let id = &Sessions::list(daemon).await.unwrap()[0].id;
  let dir = started.dir.path().join("state/sessions").join(id);
  let sockets: Vec<_> =
    std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).filter(|n| n.to_string_lossy().starts_with("v.sock")).collect();
  assert!(sockets.is_empty(), "{sockets:?}");
}

// The owner's 2026-11-10 trial: a finished or stopped session gives its
// executors back, and `session ls` then showed none at all.
#[tokio::test]
async fn a_stopped_session_lists_the_executors_it_released() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let created = Sessions::create(daemon, new_session(PROFILE, started.dir.path())).await.unwrap();
  assert!(created.released.is_empty());

  Sessions::stop(daemon, &created.id).await.unwrap();

  let stopped = Sessions::get(daemon, &created.id).await.unwrap();
  assert!(stopped.executors.is_empty(), "{:?}", stopped.executors);
  let released: Vec<_> = stopped.released.iter().map(|e| (e.name.as_str(), e.runtime.as_str())).collect();
  assert_eq!(released, [("here", "native")]);
}

#[tokio::test]
async fn a_finished_session_keeps_its_driving_programs_exit() {
  use needy_api_server::control::Guest;
  use needy_common::process::Exit;
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let created = Sessions::create(daemon, new_session(PROFILE, started.dir.path())).await.unwrap();
  assert_eq!(created.actor_exit, None);

  let exit = Exit {
    code: Some(3),
    signal: String::new(),
  };
  Guest::exit(daemon, &created.id, exit.clone()).await.unwrap();

  let finished = Sessions::get(daemon, &created.id).await.unwrap();
  assert_eq!(finished.state, SessionState::Finished);
  assert_eq!(finished.actor_exit, Some(exit));
  assert_eq!(finished.failure, None);
}

/// A profile whose every command runs on executor `far`, the remote
/// executor at `endpoint`.
fn remote_profile(endpoint: &str) -> String {
  format!(
    r#"
user: {{ name: agent, uid: 1000, gid: 1000 }}
executors:
  far: {{ runtime: remote, remote: {{ url: "{endpoint}" }} }}
commands:
  - {{ match: "**", to: far }}
"#
  )
}

/// Starts a detached `sleep 300` in `session` and returns its process ID on
/// the executor.
async fn detached_sleep(daemon: &crate::Daemon, session: &str) -> i64 {
  use needy_api_server::control::{ExecOpen, ExecOutput, ExecSpec, Processes};
  use tokio_stream::StreamExt as _;
  let spec = ExecSpec {
    argv: vec!["sleep".into(), "300".into()],
    cwd: String::new(),
    env: Default::default(),
    clear_env: false,
    tty: None,
    detach: true,
    session: session.into(),
    executor: String::new(),
    no_stdin: true,
    origin: needy_core::types::Origin::Cli,
    search_path_if_missing: false,
  };
  let mut outputs = daemon.exec(ExecOpen::Start(spec), Box::pin(tokio_stream::pending())).await.unwrap();
  loop {
    if let ExecOutput::Started(started) = outputs.next().await.unwrap().unwrap() {
      break started.pid;
    }
  }
}

/// Waits up to ten seconds for process `pid` to end.
async fn ended(pid: i64) -> bool {
  for _ in 0..200 {
    if !testing::running(pid) {
      return true;
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
  }
  false
}

// ROOT CAUSE:
//
// If a session with a remote executor stopped, its detached processes and
// its workspace mount's `sftp-server` kept running on the remote machine,
// because the `remote` provider's `destroy` does nothing and nothing else
// ended them.
//
// Before the fix, the sleep below outlived its session.
// The fix kills the processes a session started on each of its executors
// before it releases them.
#[tokio::test]
async fn stopping_a_session_ends_its_processes_on_a_remote_executor() {
  let remote = testing::Executor::start().await;
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let created = Sessions::create(daemon, new_session(&remote_profile(&remote.endpoint), started.dir.path())).await.unwrap();
  let pid = detached_sleep(daemon, &created.id).await;

  Sessions::stop(daemon, &created.id).await.unwrap();

  assert!(ended(pid).await, "process {pid} outlived its session");
}

#[tokio::test]
async fn a_finished_session_ends_its_processes_on_a_remote_executor() {
  let remote = testing::Executor::start().await;
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  fake_image(&started.dir.path().join("image"), testing::REBOOTS_AFTER_EXIT);
  let created = Sessions::create(daemon, new_actor_session(&remote_profile(&remote.endpoint), started.dir.path())).await.unwrap();
  let pid = detached_sleep(daemon, &created.id).await;
  testing::exit_actor(&started, &created.id).await;

  assert!(ended(pid).await, "process {pid} outlived its finished session");
  assert_eq!(Sessions::get(daemon, &created.id).await.unwrap().state, SessionState::Finished);
  Sessions::stop(daemon, &created.id).await.unwrap();
}

// ROOT CAUSE:
//
// If the control daemon ran on another machine than the client, a profile
// with a relative `extends` or `actor.recipe` failed with `cannot read
// recipe /Users/...`, because the client sent its profile directory and the
// daemon read the files there on its own machine.
//
// Before the fix, the daemon read the client's directory.
// The fix sends the profile and its recipe resolved, as text; the client's
// directory is gone here before the session is created.
#[tokio::test]
async fn a_session_is_created_from_a_profile_whose_directory_the_daemon_cannot_read() {
  let client = tempfile::tempdir().unwrap();
  std::fs::write(client.path().join("base.yaml"), "user: { name: agent, uid: 1000, gid: 1000 }\nexecutors:\n  here: { runtime: native }\n")
    .unwrap();
  std::fs::write(client.path().join("r.recipe.yaml"), "extends: builtin:gce-ubuntu-22.04\nos: { hostname: from-the-client }\n").unwrap();
  let yaml = "extends: base.yaml\nactor: { recipe: r.recipe.yaml }\ncommands:\n  - { match: \"**\", to: here }\n";
  let request = new_session(yaml, client.path());
  drop(client);
  let started = testing::daemon().await;

  let created = Sessions::create(&started.daemon, request).await.unwrap();

  let session = started.daemon.sessions.find(&created.id).unwrap();
  assert_eq!(session.recipe.as_ref().and_then(|r| r.hostname.as_deref()), Some("from-the-client"));
  assert_eq!(created.executors.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["here"]);
}
