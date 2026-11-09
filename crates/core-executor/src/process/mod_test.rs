use std::collections::{BTreeSet, VecDeque};
use std::time::Duration;

use super::*;

fn spec(argv: &[&str]) -> Spec {
  Spec {
    argv: argv.iter().map(|a| a.to_string()).collect(),
    ..Spec::default()
  }
}

/// Every output event of `proc`, replayed, until its exit.
async fn collect(proc: &Proc) -> (Vec<u8>, Vec<u8>, Exit) {
  let (past, mut live) = proc.subscribe(true).await;
  let mut stdout = Vec::new();
  let mut stderr = Vec::new();
  let mut events = past.into_iter().collect::<VecDeque<_>>();
  loop {
    let event = match events.pop_front() {
      Some(event) => event,
      None => live.recv().await.expect("the process exits"),
    };
    match event {
      Output::Stdout(b) => stdout.extend_from_slice(&b),
      Output::Stderr(b) => stderr.extend_from_slice(&b),
      Output::Exit(exit) => return (stdout, stderr, exit),
    }
  }
}

#[cfg(unix)]
#[tokio::test]
async fn pipes_keep_stdout_and_stderr_apart_and_return_the_exit_code() {
  let host = Host::default();
  let proc = host.spawn(spec(&["sh", "-c", "echo out; echo err >&2; exit 3"])).unwrap();

  let (stdout, stderr, exit) = collect(&proc).await;

  assert_eq!(stdout, b"out\n");
  assert_eq!(stderr, b"err\n");
  assert_eq!(
    exit,
    Exit {
      code: Some(3),
      signal: String::new()
    }
  );
  assert!(!proc.tty);
}

#[cfg(unix)]
#[tokio::test]
async fn a_client_attaching_after_the_exit_gets_the_output_replayed() {
  let host = Host::default();
  let proc = host.spawn(spec(&["sh", "-c", "echo one; echo two"])).unwrap();
  proc.wait().await;

  let attached = host.get(&proc.id).expect("a finished process can be attached");
  let (stdout, _, exit) = collect(&attached).await;

  assert_eq!(stdout, b"one\ntwo\n");
  assert_eq!(exit.code, Some(0));
}

#[cfg(unix)]
#[tokio::test]
async fn stdin_reaches_the_process_and_eof_closes_it() {
  let host = Host::default();
  let proc = host.spawn(spec(&["cat"])).unwrap();

  proc.send(Input::Stdin(b"hello\n".to_vec()));
  proc.send(Input::Eof);
  let (stdout, _, exit) = collect(&proc).await;

  assert_eq!(stdout, b"hello\n");
  assert_eq!(exit.code, Some(0));
}

#[cfg(unix)]
#[tokio::test]
async fn a_signal_by_name_ends_the_process_with_that_signal() {
  let host = Host::default();
  let proc = host.spawn(spec(&["sleep", "30"])).unwrap();

  proc.send(Input::Signal("SIGTERM".into()));
  let exit = proc.wait().await;

  assert_eq!(
    exit,
    Exit {
      code: None,
      signal: "SIGTERM".into()
    }
  );
}

#[cfg(unix)]
#[tokio::test]
async fn a_terminal_process_sees_a_terminal_and_its_signal_death() {
  let host = Host::default();
  let mut terminal = spec(&[
    "sh",
    "-c",
    "test -t 0 && test -t 1 && echo tty; kill -INT $$",
  ]);
  terminal.tty = Some((24, 80));
  let proc = host.spawn(terminal).unwrap();

  let (stdout, _, exit) = collect(&proc).await;

  assert!(proc.tty);
  assert!(String::from_utf8_lossy(&stdout).contains("tty"), "{stdout:?}");
  assert_eq!(exit.signal, "SIGINT");
}

#[tokio::test]
async fn a_missing_program_cannot_start() {
  let host = Host::default();

  let error = host.spawn(spec(&["/nonexistent/needy-test-command"])).err().expect("the program is missing");

  assert!(matches!(error, Error::Start { .. }), "{error:?}");
  assert!(error.to_string().starts_with("cannot start /nonexistent/needy-test-command"), "{error}");
  assert!(host.list().is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn a_finished_process_is_listed_until_its_retention_runs_out() {
  let host = Host::new(Duration::from_millis(300));
  let finished = host.spawn(spec(&["true"])).unwrap();
  let running = host.spawn(spec(&["sleep", "30"])).unwrap();
  finished.wait().await;

  let listed: BTreeSet<_> = host.list().iter().map(|p| p.id.clone()).collect();
  assert_eq!(listed, BTreeSet::from([finished.id.clone(), running.id.clone()]), "a process that just finished is still listed");

  tokio::time::sleep(Duration::from_millis(400)).await;
  let listed: BTreeSet<_> = host.list().iter().map(|p| p.id.clone()).collect();
  assert_eq!(listed, BTreeSet::from([running.id.clone()]), "the finished process expired; the running one stays");
  assert!(host.get(&finished.id).is_none());

  running.send(Input::Signal("SIGKILL".into()));
}

#[cfg(unix)]
#[tokio::test]
async fn shutdown_kills_running_processes_and_refuses_new_ones() {
  let host = Host::default();
  let proc = host.spawn(spec(&["sleep", "30"])).unwrap();

  tokio::time::timeout(Duration::from_secs(10), host.shutdown()).await.expect("shutdown waits for the host's tasks");

  assert_eq!(proc.exit().map(|e| e.signal), Some("SIGKILL".into()));
  assert!(matches!(host.spawn(spec(&["true"])), Err(Error::ShuttingDown)));
}
