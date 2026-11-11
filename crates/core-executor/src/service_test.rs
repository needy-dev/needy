use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use needy_api_client::exec::Event;
use needy_api_client::exec::ExecStream;
use needy_api_client::{Client, Kind};
use needy_api_proto::v1::process_service_server::ProcessServiceServer;
use needy_api_proto::v1::{ExecAttach, ExecStart, WindowSize};
use tokio_stream::wrappers::UnixListenerStream;

use super::*;

/// An executor serving `ProcessService` on a socket in a temporary
/// directory, and a client of it.
struct Executor {
  dir: tempfile::TempDir,
  client: Client,
}

async fn executor() -> Executor {
  let dir = tempfile::tempdir().unwrap();
  let socket = dir.path().join("executor.sock");
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
  Executor { dir, client }
}

/// Writes an executable script at `path` that prints `says`.
fn script(path: &Path, says: &str) {
  std::fs::create_dir_all(path.parent().unwrap()).unwrap();
  std::fs::write(path, format!("#!/bin/sh\necho {says}\n")).unwrap();
  std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Starts `program` with `PATH` set to the executor's `path` directory, in
/// which a `needy-probe` script prints `path`.
async fn start(executor: &Executor, program: &Path, search_path_if_missing: bool) -> needy_api_client::Result<String> {
  let path_dir = executor.dir.path().join("path");
  script(&path_dir.join("needy-probe"), "path");
  let start = ExecStart {
    argv: vec![program.display().to_string()],
    env: [("PATH".to_string(), path_dir.display().to_string())].into(),
    no_stdin: true,
    search_path_if_missing,
    ..Default::default()
  };
  let output = ExecStream::start(&executor.client, start).await?.run(None).await?;
  Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[tokio::test]
async fn an_absolute_program_that_exists_runs_from_its_path() {
  let executor = executor().await;
  let program = executor.dir.path().join("opt").join("needy-probe");
  script(&program, "file");

  assert_eq!(start(&executor, &program, true).await.unwrap(), "file");
}

// ROOT CAUSE:
//
// If routing kept the actor's absolute path for a program the executor has
// elsewhere, the executor could not start it; routing cut every path to its
// basename instead, which lost the path a caller named.
//
// Before the fix, the executor had no fallback of its own.
// The fix searches the executor's PATH for the basename only when the start
// asks for it and the path does not exist.
#[tokio::test]
async fn a_missing_absolute_program_runs_from_path_when_the_start_asks_for_it() {
  let executor = executor().await;
  let program = executor.dir.path().join("actor-only").join("needy-probe");

  assert_eq!(start(&executor, &program, true).await.unwrap(), "path");
}

#[tokio::test]
async fn a_missing_absolute_program_fails_naming_its_path_without_the_fallback() {
  let executor = executor().await;
  let program = executor.dir.path().join("actor-only").join("needy-probe");

  let error = start(&executor, &program, false).await.expect_err("the path is missing");

  assert_eq!(error.kind(), Kind::Failed, "{error}");
  assert!(error.to_string().starts_with(&format!("cannot start {}", program.display())), "{error}");
}

#[tokio::test]
async fn a_program_missing_from_its_path_and_from_path_fails_naming_its_path() {
  let executor = executor().await;
  let program = executor.dir.path().join("actor-only").join("needy-absent");

  let error = start(&executor, &program, true).await.expect_err("no such program anywhere");

  assert_eq!(error.kind(), Kind::Failed, "{error}");
  assert!(error.to_string().starts_with(&format!("cannot start {}", program.display())), "{error}");
}

// ROOT CAUSE:
//
// If a started process wrote its output before the stream that started it
// subscribed, that output was lost, because a new start subscribed without
// the replay buffer. The client saw the exit status with empty output.
//
// Before the fix, the stream subscribed after it sent `started`, from a task
// that could run after the process had written and exited.
// The fix subscribes the starting client with the replay buffer as soon as
// the process exists, so it receives the output from its first byte.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_started_client_receives_output_written_before_it_subscribed() {
  let executor = executor().await;

  // NOTICE: the loss needs the process to write before the stream
  // subscribes. Many concurrent starts keep the runtime busy between the two,
  // which made the loss show up in every run of this test before the fix.
  for round in 0..10 {
    let runs = (0..32).map(|_| {
      let start = ExecStart {
        argv: vec!["echo".into(), "early".into()],
        no_stdin: true,
        ..Default::default()
      };
      async { ExecStream::start(&executor.client, start).await.unwrap().run(None).await.unwrap() }
    });
    for output in futures_util::future::join_all(runs).await {
      assert_eq!(output.exit.code, Some(0), "round {round}");
      assert_eq!(String::from_utf8_lossy(&output.stdout), "early\n", "round {round}");
    }
  }
}

// ROOT CAUSE:
//
// If a client attached to a process on a terminal, the terminal kept the
// size of whoever sized it last, because an attach carried no window size
// and the client sent one only when its window changed.
//
// Before the fix, `stty size` in the attached process printed the size the
// process started with.
// The fix sends the attaching client's size with the attach, and the
// executor resizes the terminal to it.
#[tokio::test]
async fn attaching_resizes_the_terminal_to_the_attaching_client() {
  let executor = executor().await;
  let start = ExecStart {
    argv: vec!["sh".into(), "-c".into(), "read line; stty size".into()],
    tty: Some(WindowSize { rows: 24, cols: 80 }),
    detach: true,
    ..Default::default()
  };
  let mut started = ExecStream::start(&executor.client, start).await.unwrap();
  let process = loop {
    if let Some(needy_api_client::exec::Event::Started(started)) = started.next().await.unwrap() {
      break started.process_id;
    }
  };
  drop(started);

  let attached = ExecStream::attach(&executor.client, attach(&process, Some((40, 100)), false)).await.unwrap();
  let output = attached.run(Some(b"\n".to_vec())).await.unwrap();

  assert!(String::from_utf8_lossy(&output.stdout).contains("40 100"), "{:?}", String::from_utf8_lossy(&output.stdout));
}

fn attach(process: &str, window: Option<(u32, u32)>, read_only: bool) -> ExecAttach {
  ExecAttach {
    process_id: process.into(),
    replay: false,
    window: window.map(|(rows, cols)| WindowSize { rows, cols }),
    read_only,
    ..Default::default()
  }
}

/// Starts `script` on a 24x80 terminal; the stream and the process's ID.
async fn start_tty(executor: &Executor, script: &str) -> (ExecStream, String) {
  let start = ExecStart {
    argv: vec!["sh".into(), "-c".into(), script.into()],
    tty: Some(WindowSize { rows: 24, cols: 80 }),
    ..Default::default()
  };
  let mut stream = ExecStream::start(&executor.client, start).await.unwrap();
  loop {
    if let Some(Event::Started(started)) = stream.next().await.unwrap() {
      return (stream, started.process_id);
    }
  }
}

/// Reads `stream` until its output contains `marker`; all of it.
async fn read_until(stream: &mut ExecStream, marker: &str) -> String {
  let mut text = String::new();
  let read = async {
    while !text.contains(marker) {
      match stream.next().await.unwrap() {
        Some(Event::Stdout(bytes)) => text.push_str(&String::from_utf8_lossy(&bytes)),
        Some(Event::Exited(_)) | None => break,
        Some(_) => {}
      }
    }
  };
  tokio::time::timeout(std::time::Duration::from_secs(10), read).await.unwrap_or_else(|_| panic!("no {marker:?}"));
  text
}

async fn running(executor: &Executor, process: &str) -> bool {
  let list = needy_api_client::Executor::list_processes(&executor.client, Default::default()).await.unwrap();
  list.processes.iter().any(|p| p.process_id == process && p.running)
}

// The client that started a process leaves without ending it once it sent
// `detach` (G3); without it, leaving hangs the process up.
#[tokio::test]
async fn a_starting_client_that_detaches_leaves_the_process_running() {
  let executor = executor().await;
  // Both keep writing, so a hangup cannot wait for a silent process.
  let writer = "while :; do echo tick; sleep 0.05; done";
  let (detached, kept) = start_tty(&executor, writer).await;
  detached.input().detach().await.unwrap();
  // The detach is a request on the stream; it is read before the stream
  // closes.
  tokio::time::sleep(std::time::Duration::from_millis(100)).await;
  drop(detached);
  let (left, ended) = start_tty(&executor, writer).await;
  drop(left);
  tokio::time::sleep(std::time::Duration::from_millis(500)).await;

  assert!(running(&executor, &kept).await);
  assert!(!running(&executor, &ended).await);
  kill(&executor, &kept).await;
}

async fn kill(executor: &Executor, process: &str) {
  let request = needy_api_proto::v1::KillProcessRequest {
    process_id: process.into(),
    signal: "SIGKILL".into(),
  };
  needy_api_client::Executor::kill_process(&executor.client, request).await.unwrap();
}

// A read-only viewer's input never reaches the process (G6).
#[tokio::test]
async fn a_read_only_viewer_cannot_type() {
  let executor = executor().await;
  let (mut owner, process) = start_tty(&executor, "read line; echo got:$line").await;
  let viewer = ExecStream::attach(&executor.client, attach(&process, None, true)).await.unwrap();
  viewer.input().stdin(b"viewer\n".to_vec()).await.unwrap();
  tokio::time::sleep(std::time::Duration::from_millis(200)).await;
  owner.input().stdin(b"owner\n".to_vec()).await.unwrap();

  let text = read_until(&mut owner, "got:").await;
  let text = text + &read_until(&mut owner, "\n").await;
  assert!(text.contains("got:owner"), "{text:?}");
  assert!(!text.contains("viewer"), "{text:?}");
  kill(&executor, &process).await;
}

// The client that last attached or typed owns the window size; when it
// leaves, the next most recent takes over, and a read-only viewer never
// does (G6).
#[tokio::test]
async fn the_last_active_client_owns_the_window_size() {
  let executor = executor().await;
  let (mut owner, process) = start_tty(&executor, "while read line; do echo size:$(stty size):; done").await;
  let viewer = ExecStream::attach(&executor.client, attach(&process, Some((30, 100)), false)).await.unwrap();
  let watcher = ExecStream::attach(&executor.client, attach(&process, Some((10, 10)), true)).await.unwrap();
  watcher.input().resize(WindowSize { rows: 11, cols: 11 }).await.unwrap();
  owner.input().stdin(b"\n".to_vec()).await.unwrap();
  // The owner typed last: its size.
  assert!(read_until(&mut owner, "size:24 80:").await.contains("size:24 80:"));

  // A resize from a client that does not own the size waits.
  viewer
    .input()
    .resize(WindowSize {
      rows: 40,
      cols: 120,
    })
    .await
    .unwrap();
  tokio::time::sleep(std::time::Duration::from_millis(200)).await;
  owner.input().stdin(b"\n".to_vec()).await.unwrap();
  assert!(read_until(&mut owner, "size:24 80:").await.contains("size:24 80:"));

  // The viewer types: its last size.
  viewer.input().stdin(b"\n".to_vec()).await.unwrap();
  assert!(read_until(&mut owner, "size:40 120:").await.contains("size:40 120:"));

  // The owner typed again, then left after detaching: the viewer takes over.
  owner.input().stdin(b"\n".to_vec()).await.unwrap();
  read_until(&mut owner, "size:24 80:").await;
  owner.input().detach().await.unwrap();
  drop(owner);
  tokio::time::sleep(std::time::Duration::from_millis(200)).await;
  let mut viewer = viewer;
  viewer.input().stdin(b"\n".to_vec()).await.unwrap();
  assert!(read_until(&mut viewer, "size:40 120:").await.contains("size:40 120:"));
  drop(watcher);
  kill(&executor, &process).await;
}

// ROOT CAUSE:
//
// If a process wrote many small chunks while its client read more slowly
// than they came, the client's HTTP/2 connection ended with GOAWAY
// ENHANCE_YOUR_CALM `too_many_data_frames`, failing every stream on it,
// because the executor sent each chunk as a message, so as a DATA frame of
// its own, and h2 (0.4.16 and later) ends a connection on which too many
// small DATA frames wait unread. A Windows runner hit it with
// `powershell -Command 1..100000`, which writes each number and each line
// end on its own: the control plane ended the runner's connection.
//
// Before the fix, every chunk read from the process was a message.
// The fix sends a process's output at most once per batch interval, the
// chunks of one stream merged, so a burst of small writes arrives as a few
// frames.
#[tokio::test]
async fn small_writes_faster_than_the_client_reads_keep_its_connection() {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let address = listener.local_addr().unwrap();
  let service = ExecutorService {
    host: Host::default(),
    name: "test".into(),
  };
  tokio::spawn(
    tonic::transport::Server::builder()
      .add_service(ProcessServiceServer::new(service))
      .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener)),
  );
  // NOTICE: h2's budget for unread small DATA frames is half the target
  // connection window and at least 25600; a frame under 256 bytes costs 256
  // minus its length (sources at `BATCH_INTERVAL` in `service.rs`). With
  // HTTP/2's initial 65535-byte window that is about 140 frames of a few
  // bytes, so a few hundred writes show what a client's default 5 MiB
  // window shows at about ten thousand.
  let channel = tonic::transport::Endpoint::from_shared(format!("http://{address}"))
    .unwrap()
    .initial_connection_window_size(65_535)
    .connect()
    .await
    .unwrap();
  let client = Client::from_channel(channel);
  // One write per line, a short spin apart, so the executor reads each on
  // its own.
  let script = "i=0; while [ $i -lt 300 ]; do i=$((i+1)); echo $i; j=0; while [ $j -lt 50 ]; do j=$((j+1)); done; done";
  let start = ExecStart {
    argv: vec!["sh".into(), "-c".into(), script.into()],
    no_stdin: true,
    ..Default::default()
  };
  let mut stream = ExecStream::start(&client, start).await.unwrap();
  // The client reads nothing until the process is done.
  tokio::time::sleep(std::time::Duration::from_secs(1)).await;

  let mut stdout = String::new();
  let exit = loop {
    match stream.next().await {
      Ok(Some(Event::Stdout(bytes))) => stdout.push_str(&String::from_utf8_lossy(&bytes)),
      Ok(Some(Event::Exited(exit))) => break exit,
      Ok(Some(_)) => {}
      Ok(None) => panic!("the stream ended without an exit"),
      Err(e) => panic!("the stream failed after {} lines: {e}", stdout.lines().count()),
    }
  };
  assert_eq!(exit.code, Some(0));
  let expected: String = (1..=300).map(|i| format!("{i}\n")).collect();
  assert_eq!(stdout, expected);
}

// ROOT CAUSE:
//
// If the client that started a process left without detaching (it was
// SIGKILLed) and the process wrote nothing afterwards, the process kept
// running, because the executor noticed the departed client only when a
// later output failed to send.
//
// Before the fix, `sleep 300` outlived its client on pipes and terminals.
// The fix hangs the process up as soon as the client's stream closes.
#[tokio::test]
async fn a_silent_process_ends_when_its_client_leaves_without_detaching() {
  let executor = executor().await;
  let pipes = ExecStart {
    argv: vec!["sleep".into(), "300".into()],
    ..Default::default()
  };
  let mut piped = ExecStream::start(&executor.client, pipes).await.unwrap();
  let piped_id = loop {
    if let Some(Event::Started(started)) = piped.next().await.unwrap() {
      break started.process_id;
    }
  };
  let (on_terminal, terminal_id) = start_tty(&executor, "sleep 300").await;
  drop(piped);
  drop(on_terminal);

  let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
  while running(&executor, &piped_id).await || running(&executor, &terminal_id).await {
    assert!(tokio::time::Instant::now() < deadline, "a silent process outlived its client");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
  }
}
