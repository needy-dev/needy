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
