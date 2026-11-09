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
