use std::collections::HashMap;
use std::time::Duration;

use needy_api_server::control::{ControlError, ExecOpen, ExecOutput, ExecSpec, Inputs, Outputs, Processes, Runs};
use needy_common::process::Exit;
use needy_core::types::Origin;
use tokio_stream::StreamExt as _;

use crate::testing::{self, new_run};

const PROFILE: &str = r#"
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  here: { provider: native }
commands:
  - { match: "**", to: here }
"#;

fn spec(argv: &[&str], run: &str, executor: &str) -> ExecSpec {
  ExecSpec {
    argv: argv.iter().map(|a| a.to_string()).collect(),
    cwd: String::new(),
    env: HashMap::new(),
    clear_env: false,
    tty: None,
    detach: false,
    run: run.into(),
    executor: executor.into(),
    runner: String::new(),
    no_stdin: true,
    origin: Origin::Cli,
    search_path_if_missing: false,
    by: None,
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
async fn an_exec_outside_any_run_runs_on_the_named_executor() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  Runs::create(daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
  let outputs = daemon.exec(ExecOpen::Start(spec(&["echo", "hi"], "", "here")), attached()).await.unwrap();
  let (stdout, exit) = finish(outputs).await;
  assert_eq!(stdout, "hi\n");
  assert_eq!(exit.unwrap().code, Some(0));
}

// The module layout redesign's exec flow outside a run: an ambiguous name is
// an error instead of the first match.
#[tokio::test]
async fn an_exec_outside_any_run_refuses_an_ambiguous_executor_name() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  // Each run keys its own `here` instance by its ID.
  Runs::create(daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
  Runs::create(daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
  let Err(ControlError::Invalid(message)) = daemon.exec(ExecOpen::Start(spec(&["true"], "", "here")), attached()).await else {
    panic!("an ambiguous executor name must be refused");
  };
  assert!(message.contains("executor here is ambiguous"), "{message}");
}

#[tokio::test]
async fn stopping_a_run_ends_its_commands_streams() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let created = Runs::create(daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
  let mut outputs = daemon.exec(ExecOpen::Start(spec(&["sleep", "30"], &created.id, "")), attached()).await.unwrap();
  // The process runs once the relay passes on its start.
  loop {
    if let ExecOutput::Started(_) = outputs.next().await.unwrap().unwrap() {
      break;
    }
  }
  tokio::time::timeout(Duration::from_secs(10), Runs::stop(daemon, &created.id, needy_core::types::Principal::Operator))
    .await
    .expect("the stop waits for no command")
    .unwrap();
  let (_, exit) = tokio::time::timeout(Duration::from_secs(10), finish(outputs)).await.unwrap();
  assert_eq!(exit, None, "the relay ended with the run, before the command exited");
}

// ROOT CAUSE:
//
// If a run stopped while one of its commands' streams was open, the
// relay just closed the stream, because cancelling it dropped the sender;
// the caller could not tell why no exit came.
//
// Before the fix, `needy exec` took the closed stream for exit 0.
// The fix sends an explicit end with the reason before the stream closes.
#[tokio::test]
async fn a_stream_its_run_stop_ends_says_run_stopped() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let created = Runs::create(daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
  let mut outputs = daemon.exec(ExecOpen::Start(spec(&["sleep", "30"], &created.id, "")), attached()).await.unwrap();
  while !matches!(outputs.next().await.unwrap().unwrap(), ExecOutput::Started(_)) {}

  Runs::stop(daemon, &created.id, needy_core::types::Principal::Operator).await.unwrap();
  let rest: Vec<_> = tokio::time::timeout(Duration::from_secs(10), outputs.collect::<Vec<_>>()).await.unwrap();

  assert!(matches!(rest.as_slice(), [Ok(ExecOutput::Ended(reason))] if reason == "run stopped"), "{rest:?}");
}

// The owner's 2026-11-10 trial: `needy attach` forwards stdin, so attaching
// to the `sftp-server` behind an actor's workspace mount would write the
// user's keystrokes into the SFTP stream and could corrupt the mount.
#[tokio::test]
async fn attaching_to_a_workspace_mount_is_refused() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let created = Runs::create(daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
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
        window: None,
      },
      attached(),
    )
    .await;

  let Err(ControlError::Denied { message, .. }) = refused else {
    panic!("attaching to a workspace mount must be refused");
  };
  assert_eq!(message, format!("process {process} is needy's own (origin actor-mount); it cannot be attached"));
  Runs::stop(daemon, &created.id, needy_core::types::Principal::Operator).await.unwrap();
}

#[tokio::test]
async fn attaching_to_an_exited_command_replays_its_output_and_exit() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let created = Runs::create(daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
  let outputs = daemon.exec(ExecOpen::Start(spec(&["sh", "-c", "echo done; exit 4"], &created.id, "here")), attached()).await.unwrap();
  let process = Processes::list(daemon, &created.id, "").await.unwrap()[0].process.clone();
  finish(outputs).await;

  let attached = daemon
    .exec(
      ExecOpen::Attach {
        process,
        replay: true,
        window: None,
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
async fn start_detached(daemon: &crate::Daemon, argv: &[&str], run: &str, executor: &str) -> String {
  let detached = ExecSpec {
    detach: true,
    ..spec(argv, run, executor)
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
// If a run had a native executor, `needy ps` without `--run` listed
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
  let created = Runs::create(daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
  let in_run = start_detached(daemon, &["sleep", "30"], &created.id, "").await;
  let outside = start_detached(daemon, &["sleep", "30"], "", "").await;

  let listed: Vec<(String, String)> = Processes::list(daemon, "", "").await.unwrap().into_iter().map(|p| (p.process, p.executor)).collect();

  assert_eq!(listed, [(in_run, "here".to_string()), (outside, "local".to_string())]);
  Runs::stop(daemon, &created.id, needy_core::types::Principal::Operator).await.unwrap();
  daemon.shutdown().await;
}

/// The message of an exec in `run` the daemon refuses.
async fn exec_refusal(daemon: &crate::Daemon, run: &str) -> String {
  match daemon.exec(ExecOpen::Start(spec(&["true"], run, "")), attached()).await {
    Ok(_) => panic!("an exec in run {run} was accepted"),
    Err(e) => e.to_string(),
  }
}

// ROOT CAUSE:
//
// If `needy exec` named a run whose actor had finished, it said "the
// run has no executor", because finishing gives the executors back and
// placement found none; a stopped run was "no such run".
//
// Before the fix, neither message said that the run had ended.
// The fix says so, with the run's state.
#[tokio::test]
async fn an_exec_in_an_ended_run_says_it_ended_and_how() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  testing::fake_image(&started.dir.path().join("image"), testing::REBOOTS_AFTER_EXIT);
  let finished = Runs::create(daemon, testing::new_run(PROFILE, started.dir.path())).await.unwrap();
  testing::exit_actor(&started.daemon, started.dir.path(), &finished.id).await;
  let stopped = Runs::create(daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
  Runs::stop(daemon, &stopped.id, needy_core::types::Principal::Operator).await.unwrap();
  let released = async {
    while !daemon.pool.instances().await.is_empty() {
      tokio::time::sleep(Duration::from_millis(20)).await;
    }
  };
  tokio::time::timeout(Duration::from_secs(10), released).await.expect("the actor did not finish");

  assert_eq!(exec_refusal(daemon, &finished.id).await, format!("run {} has ended: finished", finished.id));
  assert_eq!(exec_refusal(daemon, &stopped.id).await, format!("run {} has ended: stopped", stopped.id));
}

// G2 of the terminal note: the daemon passes the attaching client's window
// size on to the executor.
#[tokio::test]
async fn attaching_through_the_daemon_resizes_the_terminal() {
  use needy_api_server::control::{ExecInput, WindowSize};
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let created = Runs::create(daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
  let on_a_terminal = ExecSpec {
    tty: Some(WindowSize { rows: 24, cols: 80 }),
    ..spec(&["sh", "-c", "read line; stty size"], &created.id, "")
  };
  let process = {
    let detached = ExecSpec {
      detach: true,
      ..on_a_terminal
    };
    let mut outputs = daemon.exec(ExecOpen::Start(detached), attached()).await.unwrap();
    loop {
      if let ExecOutput::Started(started) = outputs.next().await.unwrap().unwrap() {
        break started.process;
      }
    }
  };

  let input: Inputs = Box::pin(tokio_stream::iter([ExecInput::Stdin(b"\n".to_vec())]).chain(tokio_stream::pending()));
  let open = ExecOpen::Attach {
    process,
    replay: false,
    window: Some(WindowSize {
      rows: 40,
      cols: 100,
    }),
  };
  let (stdout, _) = tokio::time::timeout(Duration::from_secs(10), finish(daemon.exec(open, input).await.unwrap())).await.unwrap();

  assert!(stdout.contains("40 100"), "{stdout:?}");
  Runs::stop(daemon, &created.id, needy_core::types::Principal::Operator).await.unwrap();
}

// The terminal note's G9: the executor's fallback for an unknown `TERM` is
// recorded in the command's `exec.start` event.
#[tokio::test]
async fn a_terminal_type_the_executor_replaced_is_recorded_in_exec_start() {
  use needy_api_server::control::WindowSize;
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let created = Runs::create(daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
  let on_a_terminal = ExecSpec {
    tty: Some(WindowSize { rows: 24, cols: 80 }),
    env: [("TERM".to_string(), "needy-no-such-terminal".to_string())].into(),
    ..spec(&["true"], &created.id, "")
  };
  finish(daemon.exec(ExecOpen::Start(on_a_terminal), attached()).await.unwrap()).await;
  Runs::stop(daemon, &created.id, needy_core::types::Principal::Operator).await.unwrap();

  let trace = std::fs::read_to_string(started.dir.path().join("state/runs").join(&created.id).join("trace.jsonl")).unwrap();
  let start = trace.lines().find(|l| l.contains("\"exec.start\"")).unwrap_or_else(|| panic!("{trace}"));
  assert!(start.contains(r#""term_fallback":{"requested":"needy-no-such-terminal","used":"xterm-256color"}"#), "{start}");
}
