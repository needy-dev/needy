//! Bearer authentication in front of every method: it decides who a
//! request is from and puts that [`Caller`] into the request's extensions,
//! or refuses the request as `UNAUTHENTICATED`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tonic::Status;
use tonic::body::Body;
use tower::{Layer, Service};

use crate::control::{Caller, Credential, Runners};

/// Methods that a client may call without a token.
pub const PUBLIC_METHODS: &[&str] = &["/needy.v1.HealthService/Check"];

/// Joining needs no bearer token: the join token in the request
/// authenticates it. Public only where the runner registry is served.
pub const JOIN: &str = "/needy.v1.RunnerService/Join";

/// The methods a runner's credential reaches.
pub const RUNNER_METHODS: &[&str] = &[
  "/needy.v1.RunnerService/Heartbeat",
  "/needy.v1.RunnerService/Control",
  "/needy.v1.RunnerService/Task",
];

/// Decides who a request is from.
#[async_trait::async_trait]
pub trait Authenticator: Send + Sync + 'static {
  /// The caller of `method` that presents the bearer `token` (empty
  /// without one), or `None` to refuse the request.
  async fn authenticate(&self, method: &str, token: &str) -> Option<Caller>;
}

/// A static token, or none.
pub struct Token(pub Option<String>);

impl Token {
  /// Whether `token` is the configured one; any token is accepted when
  /// none is configured.
  pub fn accepts(&self, token: &str) -> bool {
    let Some(expected) = &self.0 else {
      return true;
    };
    // Compares every byte, so the time taken does not reveal a prefix.
    token.len() == expected.len() && token.bytes().zip(expected.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
  }
}

/// The policy of one server: its listener's caller for the control token
/// (or for any request when it has none), and a runner for a runner's
/// credential, on the runner methods only. A token shaped like a runner
/// credential that no runner holds is refused even without a control token,
/// so a removed runner learns that it was removed.
pub struct Credentials {
  pub listener: Caller,
  pub token: Token,
  pub runners: Option<Arc<dyn Runners>>,
}

#[async_trait::async_trait]
impl Authenticator for Credentials {
  async fn authenticate(&self, method: &str, token: &str) -> Option<Caller> {
    if PUBLIC_METHODS.contains(&method) || (self.runners.is_some() && method == JOIN) {
      return Some(self.listener.clone());
    }
    if self.token.0.is_some() && self.token.accepts(token) {
      return Some(self.listener.clone());
    }
    if let Some(runners) = &self.runners
      && !token.is_empty()
    {
      match runners.credential(token).await {
        Credential::Runner(name) => return RUNNER_METHODS.contains(&method).then_some(Caller::Runner { name }),
        Credential::Refused => return None,
        Credential::Other => {}
      }
    }
    self.token.0.is_none().then(|| self.listener.clone())
  }
}

/// Tower layer that identifies the caller of every request, or refuses it.
#[derive(Clone)]
pub struct AuthLayer {
  auth: Arc<dyn Authenticator>,
}

impl AuthLayer {
  pub fn new(auth: impl Authenticator) -> Self {
    Self {
      auth: Arc::new(auth),
    }
  }
}

impl<S> Layer<S> for AuthLayer {
  type Service = AuthService<S>;

  fn layer(&self, inner: S) -> Self::Service {
    AuthService {
      inner,
      auth: self.auth.clone(),
    }
  }
}

#[derive(Clone)]
pub struct AuthService<S> {
  inner: S,
  auth: Arc<dyn Authenticator>,
}

/// The bearer token of `request`, empty without one.
fn bearer<B>(request: &http::Request<B>) -> String {
  request
    .headers()
    .get(http::header::AUTHORIZATION)
    .and_then(|value| value.to_str().ok())
    .and_then(|value| value.strip_prefix("Bearer "))
    .unwrap_or_default()
    .to_string()
}

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

impl<S, B> Service<http::Request<B>> for AuthService<S>
where
  S: Service<http::Request<B>, Response = http::Response<Body>> + Clone + Send + 'static,
  S::Future: Send + 'static,
  B: Send + 'static,
{
  type Response = S::Response;
  type Error = S::Error;
  type Future = BoxFuture<Result<Self::Response, Self::Error>>;

  fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
    self.inner.poll_ready(cx)
  }

  fn call(&mut self, mut request: http::Request<B>) -> Self::Future {
    // NOTICE: the service that was polled ready serves this request, and a
    // fresh clone takes its place for the next one; the usual pattern for a
    // middleware that awaits before calling inner
    // (`https://docs.rs/tower/0.5.3/tower/trait.Service.html#be-careful-when-cloning-inner-services`).
    let fresh = self.inner.clone();
    let mut inner = std::mem::replace(&mut self.inner, fresh);
    let auth = self.auth.clone();
    Box::pin(async move {
      let token = bearer(&request);
      match auth.authenticate(request.uri().path(), &token).await {
        Some(caller) => {
          request.extensions_mut().insert(caller);
          inner.call(request).await
        }
        None => Ok(Status::unauthenticated("missing or invalid bearer token").into_http()),
      }
    })
  }
}

#[cfg(test)]
#[path = "authentication_test.rs"]
mod tests;
