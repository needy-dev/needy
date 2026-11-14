//! Bearer authentication in front of every method: it decides who a
//! request is from and puts that [`Caller`] into the request's extensions,
//! or refuses the request as `UNAUTHENTICATED`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tonic::body::Body;
use tower::{Layer, Service};

use crate::control::{Caller, Clients, Credential, Runners};

/// Methods that a client may call without a token.
pub const PUBLIC_METHODS: &[&str] = &["/needy.v1.HealthService/Check"];

/// Joining needs no bearer token: the join token in the request
/// authenticates it. Public only where the runner registry is served.
pub const JOIN: &str = "/needy.v1.RunnerService/Join";

/// Pairing needs no bearer token: the pairing code in the request
/// authenticates it. Public only where the client registry is served.
pub const PAIR: &str = "/needy.v1.ClientService/Pair";

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
  /// without one), or `None` for invalid credentials. Backend lookup failures
  /// return an error independently of credential rejection.
  async fn authenticate(&self, method: &str, token: &str) -> crate::control::Result<Option<Caller>>;
}

/// A static token, or none.
#[derive(Clone)]
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
/// (or for tokenless requests where admission allows), a runner for a runner's
/// credential, on the runner methods only, and a client for a paired
/// client's token. A token shaped like a runner credential or a client token
/// that nobody holds is refused even without a control token, so a removed
/// runner or a revoked client learns that it was.
#[derive(Clone)]
pub struct Credentials {
  pub allow_anonymous: bool,
  pub listener: Caller,
  pub token: Token,
  pub runners: Option<Arc<dyn Runners>>,
  pub clients: Option<Arc<dyn Clients>>,
}

#[async_trait::async_trait]
impl Authenticator for Credentials {
  async fn authenticate(&self, method: &str, token: &str) -> crate::control::Result<Option<Caller>> {
    if PUBLIC_METHODS.contains(&method) || (self.runners.is_some() && method == JOIN) || (self.clients.is_some() && method == PAIR) {
      return Ok(Some(self.listener.clone()));
    }
    if self.token.0.is_some() && self.token.accepts(token) {
      return Ok(Some(self.listener.clone()));
    }
    if let Some(runners) = &self.runners
      && !token.is_empty()
    {
      match runners.credential(token).await? {
        Credential::Runner(name) => return Ok(RUNNER_METHODS.contains(&method).then_some(Caller::Runner { name })),
        Credential::Refused => return Ok(None),
        Credential::Client { .. } | Credential::Other => {}
      }
    }
    if let Some(clients) = &self.clients
      && !token.is_empty()
    {
      match clients.credential(token).await? {
        // The registry grant controls administration; every client keeps
        // its named principal. Runner protocols remain runner-only.
        Credential::Client { id, name, access } => return Ok(Some(Caller::Client { id, name, access })),
        Credential::Refused => return Ok(None),
        Credential::Runner(_) | Credential::Other => {}
      }
    }
    Ok((self.allow_anonymous && self.token.0.is_none() && token.is_empty()).then(|| self.listener.clone()))
  }
}

/// Tower layer that identifies the caller of every request, or refuses it.
#[derive(Clone)]
pub struct AuthLayer {
  auth: Arc<dyn Authenticator>,
  owner_uid: Option<u32>,
}

impl AuthLayer {
  pub fn new(auth: impl Authenticator) -> Self {
    Self {
      auth: Arc::new(auth),
      owner_uid: None,
    }
  }

  /// OS admission applies only to the designated management socket.
  pub fn owner(mut self, uid: Option<u32>) -> Self {
    self.owner_uid = uid;
    self
  }
}

impl<S> Layer<S> for AuthLayer {
  type Service = AuthService<S>;

  fn layer(&self, inner: S) -> Self::Service {
    AuthService {
      inner,
      auth: self.auth.clone(),
      owner_uid: self.owner_uid,
    }
  }
}

#[derive(Clone)]
pub struct AuthService<S> {
  inner: S,
  auth: Arc<dyn Authenticator>,
  owner_uid: Option<u32>,
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
    let owner_uid = self.owner_uid;
    Box::pin(async move {
      if let Some(owner) = owner_uid {
        #[cfg(unix)]
        let admitted = request
          .extensions()
          .get::<tonic::transport::server::UdsConnectInfo>()
          .and_then(|info| info.peer_cred.as_ref())
          .is_some_and(|cred| cred.uid() == owner || cred.uid() == 0);
        #[cfg(not(unix))]
        let admitted = {
          let _ = owner;
          false
        };
        if !admitted {
          return Ok(
            crate::protocol::status::status(crate::control::ControlError::role_denied("management socket admits only its owner and root"))
              .into_http(),
          );
        }
      }
      let token = bearer(&request);
      // A present malformed/empty authorization header is an explicit
      // credential attempt. It must never become the tokenless owner path.
      if request.headers().contains_key(http::header::AUTHORIZATION)
        && (token.is_empty() || request.headers().get_all(http::header::AUTHORIZATION).iter().count() != 1)
      {
        return Ok(
          crate::protocol::status::status(
            crate::control::ControlError::Unauthenticated("missing or invalid bearer token".into())
              .with_diagnostic("auth.credential_rejected", []),
          )
          .into_http(),
        );
      }
      match auth.authenticate(request.uri().path(), &token).await {
        Ok(Some(caller)) => {
          request.extensions_mut().insert(caller);
          inner.call(request).await
        }
        Err(error) => Ok(crate::protocol::status::status(error.with_diagnostic("auth.backend_unavailable", [])).into_http()),
        Ok(None) => Ok(
          crate::protocol::status::status(
            crate::control::ControlError::Unauthenticated("missing or invalid bearer token".into())
              .with_diagnostic("auth.credential_rejected", []),
          )
          .into_http(),
        ),
      }
    })
  }
}

#[cfg(test)]
#[path = "authentication_test.rs"]
mod tests;
