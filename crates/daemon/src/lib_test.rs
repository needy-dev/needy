use std::collections::HashMap;
use std::time::Duration;

use needy_api_server::control::{ExecOpen, ExecOutput, ExecSpec, Processes, Sessions};
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
