// Only the `cfg(unix)` socket tests wait with a timeout.
#![cfg_attr(not(unix), allow(unused_imports))]

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

#[cfg(unix)]
#[tokio::test]
async fn an_over_long_socket_path_fails_to_bind_with_the_path() {
  let dir = tempfile::tempdir().unwrap();
  // Longer than `sun_path` on every Unix (104 bytes on macOS, 108 on Linux).
  let socket = dir.path().join("x".repeat(120)).join("api.sock");

  let error = Server::new(health(), Caller::Operator).bind(&[Listen::Unix(socket.clone())]).await.err().unwrap();

  assert!(error.to_string().contains(&socket.display().to_string()), "{error}");
}

#[cfg(unix)]
#[tokio::test]
async fn a_stale_socket_is_replaced() {
  let dir = tempfile::tempdir().unwrap();
  let socket = dir.path().join("run").join("api.sock");
  std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
  std::fs::write(&socket, "left by an earlier daemon").unwrap();

  let bound = Server::new(health(), Caller::Operator).bind(&[Listen::Unix(socket.clone())]).await;

  assert!(bound.is_ok(), "{:?}", bound.err());
}

#[cfg(unix)]
#[tokio::test]
async fn a_socket_where_a_server_answers_is_in_use_and_kept() {
  let dir = tempfile::tempdir().unwrap();
  let socket = dir.path().join("api.sock");
  let _live = std::os::unix::net::UnixListener::bind(&socket).unwrap();

  let error = Server::new(health(), Caller::Operator).bind(&[Listen::Unix(socket.clone())]).await.err().unwrap();

  assert!(matches!(error, Error::InUse(_)), "{error}");
  assert!(std::os::unix::net::UnixStream::connect(&socket).is_ok(), "the live socket was removed");
}

#[cfg(unix)]
#[tokio::test]
async fn a_socket_file_another_server_bound_since_is_not_removed() {
  let dir = tempfile::tempdir().unwrap();
  let socket = dir.path().join("api.sock");
  let bound = Server::new(health(), Caller::Operator).bind(&[Listen::Unix(socket.clone())]).await.unwrap();
  std::fs::remove_file(&socket).unwrap();
  let _other = std::os::unix::net::UnixListener::bind(&socket).unwrap();

  drop(bound);

  assert!(socket.exists(), "the other server's socket was removed");
}

#[tokio::test]
async fn health_needs_no_token_and_everything_else_does() {
  let served = serve_with(Caller::Operator, Some("secret")).await;

  let health = served.client.health().check(CheckRequest {}).await.unwrap().into_inner();
  let refused = served.client.runs().list_runs(ListRunsRequest {}).await.unwrap_err();
  let with_token = served.client.clone().with_token("secret").unwrap();
  let listed = with_token.runs().list_runs(ListRunsRequest {}).await;

  assert_eq!(health.role, "control");
  assert_eq!(refused.code(), Code::Unauthenticated);
  assert!(listed.is_ok(), "{:?}", listed.err());
}

#[cfg(unix)]
#[tokio::test]
async fn serving_ends_when_cancelled() {
  let dir = tempfile::tempdir().unwrap();
  let socket = dir.path().join("api.sock");
  let bound = Server::new(health(), Caller::Operator).bind(&[Listen::Unix(socket.clone())]).await.unwrap();
  let cancel = CancellationToken::new();
  let serving = tokio::spawn(bound.serve(cancel.clone()));

  cancel.cancel();

  let served = tokio::time::timeout(Duration::from_secs(5), serving).await.expect("serve did not end");
  assert!(served.unwrap().is_ok());
  assert!(!socket.exists(), "the socket outlived the server");
}

#[tokio::test]
async fn a_tcp_listener_off_loopback_needs_a_token() {
  let open = Server::new(health(), Caller::Operator).bind(&[Listen::Tcp("0.0.0.0:0".into())]).await;
  let loopback = Server::new(health(), Caller::Operator).bind(&[Listen::Tcp("127.0.0.1:0".into())]).await;
  let with_token = Server::new(health(), Caller::Operator).token(Some("secret".into())).bind(&[Listen::Tcp("0.0.0.0:0".into())]).await;

  assert!(matches!(open, Err(Error::Unauthenticated(Listen::Tcp(ref address))) if address == "0.0.0.0:0"), "{:?}", open.err());
  assert!(loopback.is_ok(), "{:?}", loopback.err());
  assert!(with_token.is_ok(), "{:?}", with_token.err());
}

#[tokio::test]
async fn a_tcp_listener_on_port_0_reports_the_port_it_got() {
  let bound = Server::new(health(), Caller::Operator).bind(&[Listen::Tcp("127.0.0.1:0".into())]).await.unwrap();

  let addresses = bound.addresses();
  let [Listen::Tcp(address)] = addresses.as_slice() else {
    panic!("{addresses:?}");
  };
  let address: std::net::SocketAddr = address.parse().unwrap();
  assert_eq!(address.ip(), std::net::Ipv4Addr::LOCALHOST);
  assert_ne!(address.port(), 0);
  assert!(std::net::TcpStream::connect(address).is_ok(), "nothing listens on {address}");
}

#[test]
fn listen_addresses_are_unix_paths_or_host_and_port() {
  assert_eq!(Listen::parse("unix:/run/needy.sock"), Listen::Unix("/run/needy.sock".into()));
  assert_eq!(Listen::parse("127.0.0.1:7000"), Listen::Tcp("127.0.0.1:7000".into()));
}
