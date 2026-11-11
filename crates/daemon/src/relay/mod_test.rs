use std::collections::HashMap;
use std::time::Duration;

use needy_api_server::control::{ControlError, ExecOpen, ExecOutput, ExecSpec, Inputs, Outputs, Processes, Runs};
use needy_common::process::Exit;
use needy_core::types::Origin;
use tokio_stream::StreamExt as _;

use crate::testing::{self, new_run};

const PROFILE: &str = r#"
actor: { runtime: firecracker }
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
        run: String::new(),
        replay: true,
        window: None,
        read_only: false,
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
        run: String::new(),
        replay: true,
        window: None,
        read_only: false,
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
    run: String::new(),
    replay: false,
    window: Some(WindowSize {
      rows: 40,
      cols: 100,
    }),
    read_only: false,
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

// G3 of the terminal note: a starting client that sends `detach` through the
// control plane leaves its process running when it goes; the daemon then
// drains the stream without it.
#[tokio::test]
async fn a_starting_client_that_detaches_through_the_daemon_leaves_the_process_running() {
  use needy_api_server::control::{ExecInput, WindowSize};
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let created = Runs::create(daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
  let on_a_terminal = ExecSpec {
    tty: Some(WindowSize { rows: 24, cols: 80 }),
    ..spec(&["sh", "-c", "while :; do echo tick; sleep 0.05; done"], &created.id, "")
  };
  let (input_tx, input_rx) = tokio::sync::mpsc::channel(4);
  let input: Inputs = Box::pin(tokio_stream::wrappers::ReceiverStream::new(input_rx));
  let mut outputs = daemon.exec(ExecOpen::Start(on_a_terminal), input).await.unwrap();
  let process = loop {
    if let ExecOutput::Started(started) = outputs.next().await.unwrap().unwrap() {
      break started.process;
    }
  };
  input_tx.send(ExecInput::Detach).await.unwrap();
  tokio::time::sleep(Duration::from_millis(100)).await;
  drop(outputs);
  drop(input_tx);
  tokio::time::sleep(Duration::from_millis(500)).await;

  let listed = Processes::list(daemon, &created.id, "").await.unwrap();
  assert!(listed.iter().any(|p| p.process == process && p.running), "{listed:?}");
  Runs::stop(daemon, &created.id, needy_core::types::Principal::Operator).await.unwrap();
}

/// A guest process service behind Firecracker's host-initiated vsock
/// handshake. The VM is a stand-in; the PTY, transport and relay are real.
async fn terminal_guest(started: &testing::Started, command: &str) -> (String, needy_core_executor::process::Host) {
  use needy_api_client::proto::v1::process_service_server::ProcessServiceServer;
  use needy_api_server::control::WindowSize;
  use needy_core_executor::process::{Host, Spec};
  use needy_core_executor::service::ExecutorService;
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  let mut request = new_run(PROFILE, started.dir.path());
  request.tty = Some(WindowSize { rows: 24, cols: 80 });
  let info = Runs::create(&started.daemon, request).await.unwrap();
  let run = started.daemon.runs.find(&info.id).unwrap();
  let path = needy_core_actor::host::firecracker::vsock(&run.dir);
  // The stand-in VM creates a file here to report it started.
  let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
  while !path.exists() {
    assert!(tokio::time::Instant::now() < deadline, "the stand-in VM did not start");
    tokio::time::sleep(Duration::from_millis(10)).await;
  }
  std::fs::remove_file(&path).unwrap();
  let listener = tokio::net::UnixListener::bind(&path).unwrap();
  let host = Host::default();
  host
    .spawn(Spec {
      argv: vec!["sh".into(), "-c".into(), command.into()],
      tty: Some((24, 80)),
      run_id: info.id.clone(),
      ..Default::default()
    })
    .unwrap();
  let incoming = tokio_stream::wrappers::UnixListenerStream::new(listener).then(|stream| async move {
    let mut stream = stream?;
    let mut request = Vec::new();
    loop {
      let byte = stream.read_u8().await?;
      request.push(byte);
      if byte == b'\n' {
        break;
      }
    }
    assert_eq!(request, format!("CONNECT {}\n", needy_core_actor::host::firecracker::PROCESS_PORT).as_bytes());
    stream.write_all(b"OK 1234\n").await?;
    Ok::<_, std::io::Error>(stream)
  });
  let service = ExecutorService {
    host: host.clone(),
    name: "actor".into(),
  };
  // The test host lives until explicit shutdown below. In particular, a
  // graceful test server must not make `run stop` wait for an open client.
  let stop = started.daemon.cancel.clone();
  started.daemon.tasks.spawn(async move {
    tonic::transport::Server::builder()
      .add_service(ProcessServiceServer::new(service))
      .serve_with_incoming_shutdown(incoming, stop.cancelled_owned())
      .await
      .unwrap();
  });
  (info.id, host)
}

fn attach_run(run: &str) -> ExecOpen {
  ExecOpen::Attach {
    process: String::new(),
    run: run.into(),
    replay: true,
    window: None,
    read_only: false,
  }
}

#[tokio::test]
async fn attaching_to_a_runs_driving_program_forwards_input_and_its_exit() {
  use needy_api_server::control::ExecInput;
  let started = testing::daemon().await;
  let (run, host) = terminal_guest(&started, "read -r line; printf 'received:%s\\n' \"$line\"; exit 7").await;
  let (tx, rx) = tokio::sync::mpsc::channel(4);
  let input: Inputs = Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx));
  let mut outputs = started.daemon.exec(attach_run(&run), input).await.unwrap();
  assert!(matches!(outputs.next().await.unwrap().unwrap(), ExecOutput::Started(_)));
  tx.send(ExecInput::Stdin(b"hello\n".to_vec())).await.unwrap();
  let (output, exit) = tokio::time::timeout(Duration::from_secs(5), finish(outputs)).await.unwrap();
  assert!(output.contains("received:hello"), "{output:?}");
  assert_eq!(exit.unwrap().code, Some(7));
  Runs::stop(&started.daemon, &run, needy_core::types::Principal::Operator).await.unwrap();
  host.shutdown().await;
}

#[tokio::test]
async fn an_interactive_create_tells_the_guest_to_wait_for_its_client() {
  use needy_api_server::control::{Guest, WindowSize};
  let started = testing::daemon().await;
  let size = WindowSize { rows: 31, cols: 97 };
  let mut request = new_run(PROFILE, started.dir.path());
  request.tty = Some(size);
  request.wait_for_attach = true;
  let run = Runs::create(&started.daemon, request).await.unwrap();
  let config = Guest::config(&started.daemon, &run.id).await.unwrap();
  assert_eq!(config.tty, Some(size));
  assert!(config.wait_for_attach);
  assert_eq!(config.process_port, needy_core_actor::host::firecracker::PROCESS_PORT);
  Runs::stop(&started.daemon, &run.id, needy_core::types::Principal::Operator).await.unwrap();
}

// ROOT CAUSE:
// An actor attach was registered under the daemon rather than its run.
// Stopping the run therefore left the relay open after its VM ended.
#[tokio::test]
async fn stopping_a_run_ends_its_driving_programs_attach() {
  let started = testing::daemon().await;
  let (run, host) = terminal_guest(&started, "read -r line").await;
  let mut outputs = started.daemon.exec(attach_run(&run), attached()).await.unwrap();
  assert!(matches!(outputs.next().await.unwrap().unwrap(), ExecOutput::Started(_)));
  Runs::stop(&started.daemon, &run, needy_core::types::Principal::Operator).await.unwrap();
  let rest = tokio::time::timeout(Duration::from_secs(2), outputs.collect::<Vec<_>>()).await.expect("attach must stop with its run");
  assert!(matches!(rest.last(), Some(Ok(ExecOutput::Ended(reason))) if reason == "run stopped"), "{rest:?}");
  assert!(!rest.iter().any(|event| matches!(event, Ok(ExecOutput::Exited(_)))), "{rest:?}");
  host.shutdown().await;
}

// A connected vsock peer can stop answering during boot. Cancellation
// must interrupt the handshake, not only the retry delay after it fails.
#[tokio::test]
async fn stopping_a_run_interrupts_an_unanswered_terminal_handshake() {
  use needy_api_server::control::WindowSize;
  let started = testing::daemon().await;
  let mut request = new_run(PROFILE, started.dir.path());
  request.tty = Some(WindowSize { rows: 24, cols: 80 });
  let info = Runs::create(&started.daemon, request).await.unwrap();
  let run = started.daemon.runs.find(&info.id).unwrap();
  let path = needy_core_actor::host::firecracker::vsock(&run.dir);
  let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
  while !path.exists() {
    assert!(tokio::time::Instant::now() < deadline);
    tokio::time::sleep(Duration::from_millis(10)).await;
  }
  std::fs::remove_file(&path).unwrap();
  let listener = tokio::net::UnixListener::bind(&path).unwrap();
  let daemon = started.daemon.clone();
  let id = info.id.clone();
  let attaching = tokio::spawn(async move { daemon.exec(attach_run(&id), attached()).await });
  let (_unanswered, _) = listener.accept().await.unwrap();
  Runs::stop(&started.daemon, &info.id, needy_core::types::Principal::Operator).await.unwrap();
  let result = tokio::time::timeout(Duration::from_secs(2), attaching).await.expect("the stopped run must interrupt its handshake").unwrap();
  assert!(matches!(result, Err(ControlError::Unavailable(_))));
}

// ROOT CAUSE:
//
// If a routed command kept the actor's working directory because the
// executor had no path map for it, and that directory did not exist on the
// executor's machine, the executor reported `cannot start /usr/bin/bash`,
// which reads like a missing program and does not say why.
//
// Before the fix, the relay passed the executor's message on unchanged.
// The fix names the directory and says that no path map maps it.
#[tokio::test]
async fn a_routed_command_whose_unmapped_cwd_is_missing_says_so_and_points_at_path_map() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let created = Runs::create(daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
  let missing = started.dir.path().join("actor-home").display().to_string();
  let routed = ExecSpec {
    cwd: missing.clone(),
    origin: Origin::ActorProxy,
    ..spec(&["sh", "-c", "true"], &created.id, "here")
  };

  let Err(ControlError::Failed(message)) = daemon.exec(ExecOpen::Start(routed), attached()).await else {
    panic!("a missing working directory cannot start");
  };

  assert!(message.starts_with(&format!("cannot start sh: working directory {missing} does not exist")), "{message}");
  assert!(message.contains("executor here has no path_map entry for it"), "{message}");
}

// ROOT CAUSE:
//
// If the client of a non-detached exec left without detaching and the
// process wrote nothing afterwards, the process kept running, because the
// daemon's relay noticed the departed client only when a later output
// failed to send, and only then dropped its stream to the executor.
//
// Before the fix, `sleep 300` outlived its client through the control plane.
// The fix drops the executor's stream as soon as the client's closes.
#[tokio::test]
async fn a_silent_process_ends_when_its_client_leaves_the_daemon_without_detaching() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let created = Runs::create(daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
  let mut outputs = daemon.exec(ExecOpen::Start(spec(&["sleep", "300"], &created.id, "")), attached()).await.unwrap();
  let process = loop {
    if let ExecOutput::Started(started) = outputs.next().await.unwrap().unwrap() {
      break started.process;
    }
  };
  drop(outputs);

  let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
  loop {
    let listed = Processes::list(daemon, &created.id, "").await.unwrap();
    if !listed.iter().any(|p| p.process == process && p.running) {
      break;
    }
    assert!(tokio::time::Instant::now() < deadline, "a silent process outlived its client: {listed:?}");
    tokio::time::sleep(Duration::from_millis(50)).await;
  }
  Runs::stop(daemon, &created.id, needy_core::types::Principal::Operator).await.unwrap();
}
