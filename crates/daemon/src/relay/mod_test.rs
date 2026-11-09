use std::collections::HashMap;
use std::time::Duration;

use needy_api_server::control::{ControlError, ExecOpen, ExecOutput, ExecSpec, Inputs, Outputs, Processes, Sessions};
use needy_common::process::Exit;
use needy_core::types::Origin;
use tokio_stream::StreamExt as _;

use crate::testing::{self, new_session};

const PROFILE: &str = r#"
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  here: { runtime: native }
commands:
  - { match: "**", to: here }
"#;

fn spec(argv: &[&str], session: &str, executor: &str) -> ExecSpec {
  ExecSpec {
    argv: argv.iter().map(|a| a.to_string()).collect(),
    cwd: String::new(),
    env: HashMap::new(),
    clear_env: false,
    tty: None,
    detach: false,
    session: session.into(),
    executor: executor.into(),
    no_stdin: true,
    origin: Origin::Cli,
  }
}

/// Input that never ends, as from a caller that stays attached.
fn attached() -> Inputs {
  Box::pin(tokio_stream::pending())
}

/// Stdout and the exit of a stream that runs to completion.
async fn finish(mut outputs: Outputs) -> (String, Option<Exit>) {
  let mut stdout = Vec::new();
  while let Some(output) = outputs.next().await {
    match output.unwrap() {
      ExecOutput::Stdout(bytes) => stdout.extend(bytes),
      ExecOutput::Exited(exit) => return (String::from_utf8(stdout).unwrap(), Some(exit)),
      _ => {}
    }
  }
  (String::from_utf8(stdout).unwrap(), None)
}

#[tokio::test]
async fn an_exec_outside_any_session_runs_on_the_named_executor() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  Sessions::create(daemon, new_session(PROFILE, started.dir.path())).await.unwrap();
  let outputs = daemon.exec(ExecOpen::Start(spec(&["echo", "hi"], "", "here")), attached()).await.unwrap();
  let (stdout, exit) = finish(outputs).await;
  assert_eq!(stdout, "hi\n");
  assert_eq!(exit.unwrap().code, Some(0));
}

// The module layout redesign's sessionless exec flow: an ambiguous name is
// an error instead of the first match.
#[tokio::test]
async fn an_exec_outside_any_session_refuses_an_ambiguous_executor_name() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  // Each session keys its own `here` instance by its ID.
  Sessions::create(daemon, new_session(PROFILE, started.dir.path())).await.unwrap();
  Sessions::create(daemon, new_session(PROFILE, started.dir.path())).await.unwrap();
  let Err(ControlError::Invalid(message)) = daemon.exec(ExecOpen::Start(spec(&["true"], "", "here")), attached()).await else {
    panic!("an ambiguous executor name must be refused");
  };
  assert!(message.contains("executor here is ambiguous"), "{message}");
}

#[tokio::test]
async fn stopping_a_session_ends_its_commands_streams() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let created = Sessions::create(daemon, new_session(PROFILE, started.dir.path())).await.unwrap();
  let mut outputs = daemon.exec(ExecOpen::Start(spec(&["sleep", "30"], &created.id, "")), attached()).await.unwrap();
  // The process runs once the relay passes on its start.
  loop {
    if let ExecOutput::Started(_) = outputs.next().await.unwrap().unwrap() {
      break;
    }
  }
  tokio::time::timeout(Duration::from_secs(10), Sessions::stop(daemon, &created.id)).await.expect("the stop waits for no command").unwrap();
  let (_, exit) = tokio::time::timeout(Duration::from_secs(10), finish(outputs)).await.unwrap();
  assert_eq!(exit, None, "the relay ended with the session, before the command exited");
}
