use std::collections::BTreeSet;
use std::path::PathBuf;

use needy_api_client::Client;
use needy_api_proto::v1::process_service_server::ProcessServiceServer;
use needy_api_proto::v1::{ExecStart, KillProcessRequest, ListProcessesRequest};
use tokio_stream::wrappers::UnixListenerStream;

use super::*;
use crate::process::Host;
use crate::service::ExecutorService;
use crate::signals::shell_code;

/// An executor serving `ProcessService` on a socket in a temporary
/// directory, and a client of it.
struct Executor {
  _dir: tempfile::TempDir,
  client: Client,
}

async fn executor() -> Executor {
  let dir = tempfile::tempdir().unwrap();
  let socket: PathBuf = dir.path().join("executor.sock");
  let listener = tokio::net::UnixListener::bind(&socket).unwrap();
  let service = ExecutorService {
    host: Host::default(),
    name: "test".into(),
  };
  tokio::spawn(
    tonic::transport::Server::builder()
      .add_service(ProcessServiceServer::new(service))
      .serve_with_incoming(UnixListenerStream::new(listener)),
  );
  let client = Client::connect(&format!("unix:{}", socket.display())).await.unwrap();
  Executor { _dir: dir, client }
}

fn start(argv: &[&str], detach: bool) -> ExecStart {
  ExecStart {
    argv: argv.iter().map(|a| a.to_string()).collect(),
    no_stdin: true,
    detach,
    ..Default::default()
  }
}

/// The stdout and exit of a stream, read without relaying to local stdio.
async fn read(mut stream: ExecStream) -> (String, Option<Exit>) {
  let mut stdout = Vec::new();
  while let Some(event) = stream.next().await.unwrap() {
    match event {
      Event::Stdout(bytes) => stdout.extend(bytes),
      Event::Exited(exit) => return (String::from_utf8_lossy(&stdout).into_owned(), Some(exit)),
      _ => {}
    }
  }
  (String::from_utf8_lossy(&stdout).into_owned(), None)
}

async fn started(stream: &mut ExecStream) -> String {
  loop {
    if let Some(Event::Started(started)) = stream.next().await.unwrap() {
      return started.process_id;
    }
  }
}

#[tokio::test]
async fn the_relay_returns_the_exit_code() {
  let executor = executor().await;
  let stream = ExecStream::start(&executor.client, start(&["sh", "-c", "exit 7"], false)).await.unwrap();

  let Ended::Exited(exit) = run(stream, Options::default()).await.unwrap() else {
    panic!("nobody detached");
  };

  assert_eq!(
    exit,
    Exit {
      code: Some(7),
      signal: String::new()
    }
  );
  assert_eq!(shell_code(&exit), 7);
}

#[tokio::test]
async fn the_relay_returns_a_signal_death_by_name() {
  let executor = executor().await;
  let stream = ExecStream::start(&executor.client, start(&["sh", "-c", "kill -TERM $$"], false)).await.unwrap();

  let Ended::Exited(exit) = run(stream, Options::default()).await.unwrap() else {
    panic!("nobody detached");
  };

  assert_eq!(
    exit,
    Exit {
      code: None,
      signal: "SIGTERM".into()
    }
  );
  assert_eq!(shell_code(&exit), 143);
}

#[tokio::test]
async fn a_detached_process_can_be_listed_attached_with_replay_and_killed() {
  let executor = executor().await;
  let mut stream = ExecStream::start(&executor.client, start(&["sh", "-c", "echo ready; exec sleep 30"], true)).await.unwrap();
  let id = started(&mut stream).await;
  drop(stream);

  // An attached client sees the output, live or replayed, and leaves the
  // process running when it goes.
  let mut attached = ExecStream::attach(
    &executor.client,
    needy_api_proto::v1::ExecAttach {
      process_id: id.clone(),
      replay: true,
      ..Default::default()
    },
  )
  .await
  .unwrap();
  started(&mut attached).await;
  let first = attached.next().await.unwrap();
  assert_eq!(first, Some(Event::Stdout(b"ready\n".to_vec())));
  drop(attached);
  let listed = executor.client.processes().list_processes(ListProcessesRequest::default()).await.unwrap().into_inner().processes;
  assert_eq!(listed.iter().map(|p| (p.process_id.clone(), p.running)).collect::<BTreeSet<_>>(), BTreeSet::from([(id.clone(), true)]));

  executor
    .client
    .processes()
    .kill_process(KillProcessRequest {
      process_id: id.clone(),
      signal: "SIGKILL".into(),
    })
    .await
    .unwrap();
  let attached = ExecStream::attach(
    &executor.client,
    needy_api_proto::v1::ExecAttach {
      process_id: id.clone(),
      replay: true,
      ..Default::default()
    },
  )
  .await
  .unwrap();
  let (replayed, exit) = read(attached).await;

  assert_eq!(replayed, "ready\n");
  assert_eq!(exit.map(|e| e.signal), Some("SIGKILL".into()));
  let listed = executor.client.processes().list_processes(ListProcessesRequest::default()).await.unwrap().into_inner().processes;
  assert!(!listed[0].running, "a killed process stays listed as finished");
}

/// Set for the child half of the stdin test below.
const STDIN_CHILD: &str = "NEEDY_RELAY_STDIN_CHILD";

/// The child half of
/// `the_relay_returns_and_leaves_stdin_alone_when_the_command_exits`: it
/// relays a command that exits at once while stdin stays open, drops its
/// runtime as the guest proxy does, and then reads a line of stdin itself.
/// Outside that test it does nothing.
#[test]
fn stdin_child() {
  if std::env::var_os(STDIN_CHILD).is_none() {
    return;
  }
  let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
  let ended = runtime.block_on(async {
    let executor = executor().await;
    let mut command = start(&["true"], false);
    command.no_stdin = false;
    let stream = ExecStream::start(&executor.client, command).await.unwrap();
    let options = Options {
      stdin: true,
      ..Options::default()
    };
    run(stream, options).await.unwrap()
  });
  println!("relayed {ended:?}");
  drop(runtime);
  let mut line = String::new();
  std::io::stdin().read_line(&mut line).unwrap();
  println!("then read {line:?}");
}

// ROOT CAUSE:
//
// If a relayed command exited while stdin stayed open with nothing to read
// (a terminal nobody types at), the relay's process did not end: the stdin
// reader was tokio's `stdin`, whose `read` runs on the runtime's blocking
// pool and cannot be interrupted, and dropping a runtime waits for that
// pool. The guest proxy drops its runtime before it exits, so a routed
// command in an interactive actor ended only at the user's next keystroke,
// and that keystroke went to the abandoned reader instead of the shell.
//
// Before the fix, the child below hung in the runtime drop until the next
// line arrived, and then lost that line.
// The fix reads stdin on a thread of its own that waits in `poll` with a
// stop pipe and reads only what is there: once the relay returns nothing
// reads stdin, so the next input stays for whoever reads it next.
#[test]
fn the_relay_returns_and_leaves_stdin_alone_when_the_command_exits() {
  let (stdin, typed) = std::io::pipe().unwrap();

  relay_then_read(stdin.into(), typed);
}

/// The same on a terminal: stdin is a pseudo-terminal nobody types at, as
/// a routed command's in an interactive actor is.
#[test]
fn the_relay_returns_and_leaves_a_terminal_alone_when_the_command_exits() {
  let pty = nix::pty::openpty(None, None).unwrap();

  relay_then_read(std::fs::File::from(pty.slave).into(), std::fs::File::from(pty.master));
}

/// Runs [`stdin_child`] with `stdin`, types a line into `typed` once its
/// relay returned, and asserts that the child read that line and exited.
fn relay_then_read(stdin: std::process::Stdio, mut typed: impl std::io::Write) {
  use std::io::BufRead;
  use std::time::{Duration, Instant};

  let mut child = std::process::Command::new(std::env::current_exe().unwrap())
    .args([
      "--exact",
      "relay::tests::stdin_child",
      "--nocapture",
      "--test-threads=1",
    ])
    .env(STDIN_CHILD, "1")
    .stdin(stdin)
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::null())
    .spawn()
    .unwrap();
  let (lines_tx, lines) = std::sync::mpsc::channel();
  let stdout = child.stdout.take().unwrap();
  std::thread::spawn(move || {
    for line in std::io::BufReader::new(stdout).lines() {
      let Ok(line) = line else { return };
      if lines_tx.send(line).is_err() {
        return;
      }
    }
  });
  let wait = |want: &str| {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
      match lines.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        // The harness prints the test's name on the same line first.
        Ok(line) if line.contains(want) => return line.find(want).map(|at| line[at..].to_string()),
        Ok(_) => continue,
        Err(_) => return None,
      }
    }
  };

  let relayed = wait("relayed");
  // The relay has returned; this line is for whoever reads stdin next.
  typed.write_all(b"next\n").unwrap();
  let then = wait("then read");
  let exited = exits_within(&mut child, Duration::from_secs(10));
  drop(typed);

  assert!(relayed.is_some_and(|line| line.contains("Exited")), "the relay did not return");
  assert_eq!(then.as_deref(), Some(r#"then read "next\n""#), "the line typed after the command exited was lost");
  assert!(exited, "the process did not end after the relay");
}

/// Whether `child` exits within `limit`; it is killed otherwise.
fn exits_within(child: &mut std::process::Child, limit: std::time::Duration) -> bool {
  let deadline = std::time::Instant::now() + limit;
  while std::time::Instant::now() < deadline {
    if child.try_wait().unwrap().is_some() {
      return true;
    }
    std::thread::sleep(std::time::Duration::from_millis(20));
  }
  // A child that hung is the failure reported below; killing it only
  // cleans up.
  let _ = child.kill();
  let _ = child.wait();
  false
}
