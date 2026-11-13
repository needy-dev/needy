use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use needy_api_proto::v1::health_service_server::{HealthService, HealthServiceServer};
use needy_api_proto::v1::{CheckRequest, CheckResponse};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Request, Response, Status};

use crate::{Client, Error};

struct Health;

#[tonic::async_trait]
impl HealthService for Health {
  async fn check(&self, _: Request<CheckRequest>) -> Result<Response<CheckResponse>, Status> {
    Ok(Response::new(CheckResponse {
      role: "control".into(),
      ..Default::default()
    }))
  }
}

/// A gRPC server on a loopback TCP port, counting the connections it
/// accepts.
async fn server() -> (u16, Arc<AtomicUsize>) {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  let accepted = Arc::new(AtomicUsize::new(0));
  let counted = accepted.clone();
  let incoming = tokio_stream::StreamExt::map(TcpListenerStream::new(listener), move |connection| {
    counted.fetch_add(1, Ordering::SeqCst);
    connection
  });
  tokio::spawn(tonic::transport::Server::builder().add_service(HealthServiceServer::new(Health)).serve_with_incoming(incoming));
  (port, accepted)
}

/// An endpoint command that relays stdio to `port`, as `cloudflared access
/// tcp` or `ssh -W` would, and writes its PID to `pid_file`.
// NOTICE: bash's `/dev/tcp` instead of nc or socat, which CI runners and
// developer machines do not all have; bash is on both macOS and Linux.
// Reference: https://www.gnu.org/software/bash/manual/html_node/Redirections.html
// (read 2026-11-13: `/dev/tcp/host/port` opens a TCP connection).
fn relay(port: u16, pid_file: &std::path::Path) -> String {
  format!("exec:bash -c 'echo $$ > {}; exec 3<>/dev/tcp/127.0.0.1/{port}; cat <&3 & exec cat >&3'", pid_file.display())
}

fn alive(pid: i32) -> bool {
  // SAFETY: signal 0 only checks that the process exists.
  unsafe { libc::kill(pid, 0) == 0 }
}

#[tokio::test]
async fn a_call_reaches_the_server_through_the_endpoint_command() {
  let (port, accepted) = server().await;
  let dir = tempfile::tempdir().unwrap();

  let client = Client::connect(&relay(port, &dir.path().join("pid"))).await.unwrap();
  let health = client.health().check(CheckRequest {}).await.unwrap().into_inner();

  assert_eq!(health.role, "control");
  assert_eq!(accepted.load(Ordering::SeqCst), 1, "one connection, through one command");
}

#[tokio::test]
async fn dropping_the_client_ends_the_endpoint_command() {
  let (port, _) = server().await;
  let dir = tempfile::tempdir().unwrap();
  let pid_file = dir.path().join("pid");
  let client = Client::connect(&relay(port, &pid_file)).await.unwrap();
  client.health().check(CheckRequest {}).await.unwrap();
  let pid: i32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
  assert!(alive(pid));

  drop(client);

  let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
  while alive(pid) && tokio::time::Instant::now() < deadline {
    tokio::time::sleep(Duration::from_millis(50)).await;
  }
  assert!(!alive(pid), "the endpoint command {pid} outlived its connection");
}

#[tokio::test]
async fn an_endpoint_command_that_cannot_start_names_the_program() {
  let error = Client::connect("exec:needy-no-such-endpoint-command --flag").await.unwrap_err();

  assert!(format!("{error:?}").contains("needy-no-such-endpoint-command"), "{error:?}");
}

#[tokio::test]
async fn an_empty_or_unbalanced_endpoint_command_is_invalid() {
  for endpoint in ["exec:", "exec:   ", "exec:ssh -W 'host:7400"] {
    let error = Client::connect(endpoint).await.unwrap_err();
    assert!(matches!(error, Error::InvalidEndpoint(_)), "{endpoint}: {error:?}");
  }
}
