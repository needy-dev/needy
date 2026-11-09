use std::collections::HashMap;
use std::time::Duration;

use needy_api_server::control::{ControlError, ExecOpen, ExecOutput, ExecSpec, Processes, Sessions};
use needy_core::types::{Origin, SessionState};
use tokio_stream::StreamExt as _;

use crate::testing::{self, new_session, running};

const PROFILE: &str = r#"
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  here: { runtime: native }
commands:
  - { match: "**", to: here }
"#;

#[tokio::test]
async fn shutdown_stops_sessions_and_the_local_executors_processes() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let first = Sessions::create(daemon, new_session(PROFILE, started.dir.path())).await.unwrap();
  let second = Sessions::create(daemon, new_session(PROFILE, started.dir.path())).await.unwrap();
  let spec = ExecSpec {
    argv: vec!["sleep".into(), "30".into()],
    cwd: String::new(),
    env: HashMap::new(),
    clear_env: false,
    tty: None,
    detach: true,
    session: String::new(),
    executor: String::new(),
    no_stdin: true,
    origin: Origin::Cli,
    search_path_if_missing: false,
  };
  let mut outputs = daemon.exec(ExecOpen::Start(spec), Box::pin(tokio_stream::pending())).await.unwrap();
  let pid = loop {
    if let ExecOutput::Started(started) = outputs.next().await.unwrap().unwrap() {
      break started.pid;
    }
  };
  // A detached process outlives its caller.
  drop(outputs);
  assert!(running(pid));

  tokio::time::timeout(Duration::from_secs(20), daemon.shutdown()).await.expect("shutdown waits for every task");

  let states: Vec<_> = Sessions::list(daemon).await.unwrap().into_iter().map(|s| (s.id, s.state)).collect();
  assert_eq!(
    states,
    [
      (first.id, SessionState::Stopped),
      (second.id, SessionState::Stopped)
    ]
  );
  assert!(!running(pid), "the local executor's processes are killed");
}

#[tokio::test]
async fn shutdown_ends_open_streams_saying_the_daemon_is_shutting_down() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let session = Sessions::create(daemon, new_session(PROFILE, started.dir.path())).await.unwrap();
  let mut streams = Vec::new();
  for session in [String::new(), session.id] {
    let spec = ExecSpec {
      argv: vec!["sleep".into(), "30".into()],
      cwd: String::new(),
      env: HashMap::new(),
      clear_env: false,
      tty: None,
      detach: false,
      session,
      executor: String::new(),
      no_stdin: true,
      origin: Origin::Cli,
      search_path_if_missing: false,
    };
    let mut outputs = daemon.exec(ExecOpen::Start(spec), Box::pin(tokio_stream::pending())).await.unwrap();
    while !matches!(outputs.next().await.unwrap().unwrap(), ExecOutput::Started(_)) {}
    streams.push(outputs);
  }

  tokio::time::timeout(Duration::from_secs(20), daemon.shutdown()).await.expect("shutdown waits for every task");

  // A session's stream and one outside any session say the same.
  for outputs in streams {
    let rest: Vec<_> = outputs.collect().await;
    assert!(matches!(rest.as_slice(), [Ok(ExecOutput::Ended(reason))] if reason == "daemon shutting down"), "{rest:?}");
  }
}

// A socket path longer than `sun_path` used to make the control daemon wait
// 10 s for its local executor before it failed without naming the path.
// This keeps start-up failing at once with the path in the error.
#[tokio::test]
async fn a_runtime_directory_too_long_for_a_socket_fails_start_up_at_once() {
  let dir = tempfile::tempdir().unwrap();
  let runtime_dir = dir.path().join("r".repeat(120));
  let config = crate::Config {
    state_dir: dir.path().join("state"),
    runtime_dir: runtime_dir.clone(),
    actor_image: dir.path().join("image"),
    docker_image: "needy-executor:latest".into(),
  };

  let started = tokio::time::timeout(Duration::from_secs(2), crate::Daemon::start(config)).await.expect("start-up fails at once");

  let Err(error) = started else {
    panic!("a daemon started on a socket path over sun_path");
  };
  let socket = runtime_dir.join("local.sock");
  assert!(format!("{error:#}").contains(&socket.display().to_string()), "{error:#}");
}

// ROOT CAUSE:
//
// If the daemon shut down while a session was being created, the create
// was not unwound, because shutdown only stopped sessions it could move to
// `stopping` and nothing told the create to stop.
//
// Before the fix, the create went on after shutdown returned and ended
// `running`, holding its executors (for Docker, a container) with no
// daemon left to release them.
// The fix runs the create under its session's token, which shutdown
// cancels and waits for: the create unwinds through its rollback and
// records `session.fail` saying the daemon shut down.
#[tokio::test]
async fn shutdown_unwinds_a_session_being_created() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  // `b`'s executor does not listen yet, so the create waits for it after
  // it acquired `a`.
  let late = started.dir.path().join("late.sock");
  let profile = format!(
    r#"
user: {{ name: agent, uid: 1000, gid: 1000 }}
executors:
  a: {{ runtime: native }}
  b: {{ runtime: remote, remote: {{ url: "unix:{}" }} }}
commands:
  - {{ match: "**", to: a }}
"#,
    late.display()
  );
  let creating = tokio::spawn({
    let daemon = daemon.clone();
    let request = new_session(&profile, started.dir.path());
    async move { Sessions::create(&daemon, request).await }
  });
  while daemon.pool.instances().await.is_empty() {
    tokio::time::sleep(Duration::from_millis(20)).await;
  }

  let shutdown = tokio::spawn({
    let daemon = daemon.clone();
    async move { daemon.shutdown().await }
  });
  tokio::time::sleep(Duration::from_millis(200)).await;
  let _remote = testing::Executor::start_at(&late).await;
  let created = tokio::time::timeout(Duration::from_secs(20), creating).await.expect("the create ends").unwrap();
  tokio::time::timeout(Duration::from_secs(20), shutdown).await.expect("shutdown ends").unwrap();

  let Err(ControlError::Failed(message)) = created else {
    panic!("a create the daemon's shutdown interrupted must fail, not {created:?}");
  };
  assert!(message.contains("the daemon shut down"), "{message}");
  assert!(daemon.pool.instances().await.is_empty(), "the unwound create still holds an executor");
  let listed = Sessions::list(daemon).await.unwrap();
  assert_eq!(listed.len(), 1);
  assert_eq!(listed[0].state, SessionState::Failed);
  let trace = needy_trace::sink::sqlite::load(&started.dir.path().join("state/trace.db"), &listed[0].id).await.unwrap();
  let last = trace.events.last().unwrap();
  assert_eq!(last.kind, "session.fail");
  assert!(last.payload.get().contains("the daemon shut down"), "{}", last.payload.get());
}
