//! Protocol adapters for serving the Needy daemon API.
//!
//! The daemon implements the generated service traits re-exported from
//! [`proto`]. This crate adds what every listener needs: bearer
//! authentication in front of every method except health checks.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tonic::Status;
use tonic::body::Body;
use tower::{Layer, Service};

pub use needy_api_proto as proto;

pub mod protocol;

/// Methods that a client may call without a token by default.
pub const PUBLIC_METHODS: &[&str] = &["/needy.v1.HealthService/Check"];

/// Checks a bearer token. A request without one is checked as the empty
/// token. The daemon owns token storage and revocation.
pub trait Authenticator: Send + Sync + 'static {
  fn authenticate(&self, token: &str) -> bool;
}

/// Tower layer that rejects requests without a valid bearer token.
#[derive(Clone)]
pub struct AuthLayer {
  auth: Arc<dyn Authenticator>,
  public: &'static [&'static str],
}

impl AuthLayer {
  pub fn new(auth: impl Authenticator) -> Self {
    Self {
      auth: Arc::new(auth),
      public: PUBLIC_METHODS,
    }
  }

  /// Replaces the set of methods that need no token.
  pub fn with_public(mut self, public: &'static [&'static str]) -> Self {
    self.public = public;
    self
  }
}

impl<S> Layer<S> for AuthLayer {
  type Service = AuthService<S>;

  fn layer(&self, inner: S) -> Self::Service {
    AuthService {
      inner,
      auth: self.auth.clone(),
      public: self.public,
    }
  }
}

#[derive(Clone)]
pub struct AuthService<S> {
  inner: S,
  auth: Arc<dyn Authenticator>,
  public: &'static [&'static str],
}

impl<S> AuthService<S> {
  fn allows<B>(&self, request: &http::Request<B>) -> bool {
    if self.public.contains(&request.uri().path()) {
      return true;
    }
    request
      .headers()
      .get(http::header::AUTHORIZATION)
      .and_then(|value| value.to_str().ok())
      .and_then(|value| value.strip_prefix("Bearer "))
      .map_or_else(|| self.auth.authenticate(""), |token| self.auth.authenticate(token))
  }
}

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

impl<S, B> Service<http::Request<B>> for AuthService<S>
where
  S: Service<http::Request<B>, Response = http::Response<Body>>,
  S::Future: Send + 'static,
{
  type Response = S::Response;
  type Error = S::Error;
  type Future = BoxFuture<Result<Self::Response, Self::Error>>;

  fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
    self.inner.poll_ready(cx)
  }

  fn call(&mut self, request: http::Request<B>) -> Self::Future {
    if self.allows(&request) {
      Box::pin(self.inner.call(request))
    } else {
      let response = Status::unauthenticated("missing or invalid bearer token").into_http();
      Box::pin(async move { Ok(response) })
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use needy_api_client::Client;
  use proto::v1::health_service_server::{HealthService, HealthServiceServer};
  use proto::v1::{CheckRequest, CheckResponse};
  use tokio::net::TcpListener;
  use tokio_stream::wrappers::TcpListenerStream;
  use tonic::{Code, Request, Response};

  struct Token;

  impl Authenticator for Token {
    fn authenticate(&self, token: &str) -> bool {
      token == "secret"
    }
  }

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

  #[tokio::test]
  async fn methods_need_a_token() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(
      tonic::transport::Server::builder()
        .layer(AuthLayer::new(Token).with_public(&[]))
        .add_service(HealthServiceServer::new(Health))
        .serve_with_incoming(TcpListenerStream::new(listener)),
    );

    let client = Client::connect(&url).await.unwrap();
    let err = client.health().check(CheckRequest {}).await.unwrap_err();
    assert_eq!(err.code(), Code::Unauthenticated);

    let client = client.with_token("secret").unwrap();
    let health = client.health().check(CheckRequest {}).await.unwrap();
    assert_eq!(health.into_inner().role, "control");
  }
}
