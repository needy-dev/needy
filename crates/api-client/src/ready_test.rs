use std::time::{Duration, Instant};

use needy_api_proto::v1::health_service_server::{HealthService, HealthServiceServer};
use needy_api_proto::v1::{CheckRequest, CheckResponse};
use tokio_stream::wrappers::UnixListenerStream;
use tonic::{Request, Response, Status};

use super::*;

struct Health;

#[tonic::async_trait]
impl HealthService for Health {
  async fn check(&self, _: Request<CheckRequest>) -> Result<Response<CheckResponse>, Status> {
    Ok(Response::new(CheckResponse {
      role: "executor".into(),
      ..Default::default()
    }))
  }
}

#[tokio::test]
async fn waits_for_a_server_that_binds_its_socket_late() {
  let dir = tempfile::tempdir().unwrap();
  let socket = dir.path().join("late.sock");
  let late = socket.clone();
  tokio::spawn(async move {
    tokio::time::sleep(Duration::from_millis(300)).await;
    let listener = tokio::net::UnixListener::bind(&late).unwrap();
    tonic::transport::Server::builder()
      .add_service(HealthServiceServer::new(Health))
      .serve_with_incoming(UnixListenerStream::new(listener))
      .await
      .unwrap();
  });

  let client = wait(&format!("unix:{}", socket.display()), Duration::from_secs(5)).await.unwrap();

  let health = client.health().check(CheckRequest {}).await.unwrap().into_inner();
  assert_eq!(health.role, "executor");
}

#[tokio::test]
async fn gives_up_after_the_deadline_with_the_endpoint() {
  let dir = tempfile::tempdir().unwrap();
  let endpoint = format!("unix:{}", dir.path().join("never.sock").display());

  let error = wait(&endpoint, Duration::from_millis(300)).await.unwrap_err();

  assert!(matches!(&error, Error::Unreachable { endpoint: e, .. } if *e == endpoint), "{error:?}");
}

#[tokio::test]
async fn a_malformed_endpoint_fails_at_once_instead_of_retrying() {
  let started = Instant::now();

  let error = wait("ftp://example.com", Duration::from_secs(10)).await.unwrap_err();

  assert!(matches!(error, Error::InvalidEndpoint(_)), "{error:?}");
  assert!(started.elapsed() < Duration::from_secs(1), "took {:?}", started.elapsed());
}
