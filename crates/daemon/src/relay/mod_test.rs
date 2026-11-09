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
    search_path_if_missing: false,
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

// ROOT CAUSE:
//
// If a session stopped while one of its commands' streams was open, the
// relay just closed the stream, because cancelling it dropped the sender;
// the caller could not tell why no exit came.
//
// Before the fix, `needy exec` took the closed stream for exit 0.
// The fix sends an explicit end with the reason before the stream closes.
#[tokio::test]
async fn a_stream_its_session_stop_ends_says_session_stopped() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let created = Sessions::create(daemon, new_session(PROFILE, started.dir.path())).await.unwrap();
  let mut outputs = daemon.exec(ExecOpen::Start(spec(&["sleep", "30"], &created.id, "")), attached()).await.unwrap();
  while !matches!(outputs.next().await.unwrap().unwrap(), ExecOutput::Started(_)) {}

  Sessions::stop(daemon, &created.id).await.unwrap();
  let rest: Vec<_> = tokio::time::timeout(Duration::from_secs(10), outputs.collect::<Vec<_>>()).await.unwrap();

  assert!(matches!(rest.as_slice(), [Ok(ExecOutput::Ended(reason))] if reason == "session stopped"), "{rest:?}");
}

// The owner's 2026-11-10 trial: `needy attach` forwards stdin, so attaching
// to the `sftp-server` behind an actor's workspace mount would write the
// user's keystrokes into the SFTP stream and could corrupt the mount.
#[tokio::test]
async fn attaching_to_a_workspace_mount_is_refused() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let created = Sessions::create(daemon, new_session(PROFILE, started.dir.path())).await.unwrap();
  let mount = ExecSpec {
    origin: Origin::ActorMount,
    detach: true,
    ..spec(&["sleep", "30"], &created.id, "here")
  };
  let mut outputs = daemon.exec(ExecOpen::Start(mount), attached()).await.unwrap();
  let process = loop {
    if let ExecOutput::Started(started) = outputs.next().await.unwrap().unwrap() {
      break started.process;
    }
  };

  let refused = daemon
    .exec(
      ExecOpen::Attach {
        process: process.clone(),
        replay: true,
      },
      attached(),
    )
    .await;

  let Err(ControlError::Denied { message, .. }) = refused else {
    panic!("attaching to a workspace mount must be refused");
  };
  assert_eq!(message, format!("process {process} is needy's own (origin actor-mount); it cannot be attached"));
  Sessions::stop(daemon, &created.id).await.unwrap();
}

#[tokio::test]
async fn attaching_to_an_exited_command_replays_its_output_and_exit() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let created = Sessions::create(daemon, new_session(PROFILE, started.dir.path())).await.unwrap();
  let outputs = daemon.exec(ExecOpen::Start(spec(&["sh", "-c", "echo done; exit 4"], &created.id, "here")), attached()).await.unwrap();
  let process = Processes::list(daemon, &created.id).await.unwrap()[0].process.clone();
  finish(outputs).await;

  let attached = daemon
    .exec(
      ExecOpen::Attach {
        process,
        replay: true,
      },
      attached(),
    )
    .await
    .unwrap();

  let (stdout, exit) = tokio::time::timeout(Duration::from_secs(10), finish(attached)).await.unwrap();
  assert_eq!(stdout, "done\n");
  assert_eq!(exit.unwrap().code, Some(4));
}

/// Starts `argv` detached and returns its process ID once it runs.
async fn start_detached(daemon: &crate::Daemon, argv: &[&str], session: &str, executor: &str) -> String {
  let detached = ExecSpec {
    detach: true,
    ..spec(argv, session, executor)
  };
  let mut outputs = daemon.exec(ExecOpen::Start(detached), attached()).await.unwrap();
  loop {
    if let ExecOutput::Started(started) = outputs.next().await.unwrap().unwrap() {
      break started.process;
    }
  }
}

// ROOT CAUSE:
//
// If a session had a native executor, `needy ps` without `--session` listed
// each of its processes twice, under `here` and under `local`, because both
// instances share this machine's executor and each was asked for every
// process on it.
//
// Before the fix, the list had one row per instance that shares the executor.
// The fix asks each executor once and names a process by the instance it
// was started through.
#[tokio::test]
async fn processes_on_an_executor_two_instances_share_are_listed_once() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let created = Sessions::create(daemon, new_session(PROFILE, started.dir.path())).await.unwrap();
  let in_session = start_detached(daemon, &["sleep", "30"], &created.id, "").await;
  let outside = start_detached(daemon, &["sleep", "30"], "", "").await;

  let listed: Vec<(String, String)> = Processes::list(daemon, "").await.unwrap().into_iter().map(|p| (p.process, p.executor)).collect();

  assert_eq!(
    listed,
    [
      (in_session, "here".to_string()),
      (outside, "local".to_string())
    ]
  );
  Sessions::stop(daemon, &created.id).await.unwrap();
  daemon.shutdown().await;
}
