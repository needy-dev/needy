use needy_api_client::Client;
use needy_api_proto::v1::health_service_server::{HealthService, HealthServiceServer};
use needy_api_proto::v1::{CheckRequest, CheckResponse};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Code, Request, Response};

use super::*;

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

/// Accepts `secret` only, for every method.
struct Secret;

#[async_trait::async_trait]
impl Authenticator for Secret {
  async fn authenticate(&self, _method: &str, token: &str) -> Option<Caller> {
    (token == "secret").then_some(Caller::Cli)
  }
}

#[tokio::test]
async fn methods_need_a_token() {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let url = format!("http://{}", listener.local_addr().unwrap());
  tokio::spawn(
    tonic::transport::Server::builder()
      .layer(AuthLayer::new(Secret))
      .add_service(HealthServiceServer::new(Health))
      .serve_with_incoming(TcpListenerStream::new(listener)),
  );

  let client = Client::connect(&url).await.unwrap();
  let err = client.health().check(CheckRequest {}).await.unwrap_err();
  assert_eq!(err.code(), Code::Unauthenticated);

  let wrong = client.clone().with_token("guess").unwrap();
  let err = wrong.health().check(CheckRequest {}).await.unwrap_err();
  assert_eq!(err.code(), Code::Unauthenticated);

  let client = client.with_token("secret").unwrap();
  let health = client.health().check(CheckRequest {}).await.unwrap();
  assert_eq!(health.into_inner().role, "control");
}

#[test]
fn no_configured_token_accepts_any() {
  assert!(Token(None).accepts(""));
  assert!(Token(None).accepts("anything"));
  assert!(!Token(Some("secret".into())).accepts("secreT"));
  assert!(!Token(Some("secret".into())).accepts("secret2"));
}
