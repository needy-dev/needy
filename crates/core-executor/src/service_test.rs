use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use needy_api_client::exec::ExecStream;
use needy_api_client::{Client, Kind};
use needy_api_proto::v1::ExecStart;
use needy_api_proto::v1::process_service_server::ProcessServiceServer;
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
