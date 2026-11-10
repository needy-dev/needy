use std::time::Duration;

use needy_api_proto::v1::{CheckRequest, ListRunsRequest};
use tonic::Code;

use super::*;
use crate::testing::serve_with;

fn health() -> Health {
  Health {
    instance_id: "test".into(),
    role: "executor",
  }
}

#[tokio::test]
async fn an_over_long_socket_path_fails_to_bind_with_the_path() {
  let dir = tempfile::tempdir().unwrap();
  // Longer than `sun_path` on every Unix (104 bytes on macOS, 108 on Linux).
  let socket = dir.path().join("x".repeat(120)).join("api.sock");

  let error = Server::new(health(), Caller::Cli).bind(&[Listen::Unix(socket.clone())]).await.err().unwrap();

  assert!(error.to_string().contains(&socket.display().to_string()), "{error}");
}

#[tokio::test]
async fn a_stale_socket_is_replaced() {
  let dir = tempfile::tempdir().unwrap();
  let socket = dir.path().join("run").join("api.sock");
  std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
  std::fs::write(&socket, "left by an earlier daemon").unwrap();

  let bound = Server::new(health(), Caller::Cli).bind(&[Listen::Unix(socket.clone())]).await;

  assert!(bound.is_ok(), "{:?}", bound.err());
}

#[tokio::test]
async fn health_needs_no_token_and_everything_else_does() {
  let served = serve_with(Caller::Cli, Some("secret")).await;

  let health = served.client.health().check(CheckRequest {}).await.unwrap().into_inner();
  let refused = served.client.runs().list_runs(ListRunsRequest {}).await.unwrap_err();
  let with_token = served.client.clone().with_token("secret").unwrap();
  let listed = with_token.runs().list_runs(ListRunsRequest {}).await;

  assert_eq!(health.role, "control");
  assert_eq!(refused.code(), Code::Unauthenticated);
  assert!(listed.is_ok(), "{:?}", listed.err());
}

#[tokio::test]
async fn serving_ends_when_cancelled() {
  let dir = tempfile::tempdir().unwrap();
  let socket = dir.path().join("api.sock");
  let bound = Server::new(health(), Caller::Cli).bind(&[Listen::Unix(socket)]).await.unwrap();
  let cancel = CancellationToken::new();
  let serving = tokio::spawn(bound.serve(cancel.clone()));

  cancel.cancel();

  let served = tokio::time::timeout(Duration::from_secs(5), serving).await.expect("serve did not end");
  assert!(served.unwrap().is_ok());
}

#[tokio::test]
async fn a_tcp_listener_off_loopback_needs_a_token() {
  let open = Server::new(health(), Caller::Cli).bind(&[Listen::Tcp("0.0.0.0:0".into())]).await;
  let loopback = Server::new(health(), Caller::Cli).bind(&[Listen::Tcp("127.0.0.1:0".into())]).await;
  let with_token = Server::new(health(), Caller::Cli).token(Some("secret".into())).bind(&[Listen::Tcp("0.0.0.0:0".into())]).await;

  assert!(matches!(open, Err(Error::Unauthenticated(Listen::Tcp(ref address))) if address == "0.0.0.0:0"), "{:?}", open.err());
  assert!(loopback.is_ok(), "{:?}", loopback.err());
  assert!(with_token.is_ok(), "{:?}", with_token.err());
}

#[test]
fn listen_addresses_are_unix_paths_or_host_and_port() {
  assert_eq!(Listen::parse("unix:/run/needy.sock"), Listen::Unix("/run/needy.sock".into()));
  assert_eq!(Listen::parse("127.0.0.1:7000"), Listen::Tcp("127.0.0.1:7000".into()));
}
