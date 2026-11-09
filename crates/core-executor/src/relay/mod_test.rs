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

  let exit = run(stream, false, false).await.unwrap();

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

  let exit = run(stream, false, false).await.unwrap();

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
  let mut attached = ExecStream::attach(&executor.client, &id, true).await.unwrap();
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
  let attached = ExecStream::attach(&executor.client, &id, true).await.unwrap();
  let (replayed, exit) = read(attached).await;

  assert_eq!(replayed, "ready\n");
  assert_eq!(exit.map(|e| e.signal), Some("SIGKILL".into()));
  let listed = executor.client.processes().list_processes(ListProcessesRequest::default()).await.unwrap().into_inner().processes;
  assert!(!listed[0].running, "a killed process stays listed as finished");
}
