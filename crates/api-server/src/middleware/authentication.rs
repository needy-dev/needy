//! Bearer authentication in front of every method: it decides who a
//! request is from and puts that [`Caller`] into the request's extensions,
//! or refuses the request as `UNAUTHENTICATED`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tonic::body::Body;
use tower::{Layer, Service};

use crate::control::{Caller, Clients, Credential, Runners, Runs};

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

/// Metadata header carried by an actor runner when calling [`ACTOR_METHODS`]
/// on behalf of a run bound to it (Node Authorizer).
pub const RUN_ID_HEADER: &str = "x-needy-run-id";

/// The explicit allowlist of methods an actor may call, whether on a run's
/// local guest listener or delegated through its bound actor runner.
pub const ACTOR_METHODS: &[&str] = &[
  "/needy.v1.GuestService/GetConfig",
  "/needy.v1.GuestService/PushEvents",
  "/needy.v1.GuestService/ReportExit",
  "/needy.v1.GuestService/FetchFiles",
  "/needy.v1.GuestService/PushFiles",
  "/needy.v1.GuestService/PushLayer",
  "/needy.v1.GuestService/Control",
  "/needy.v1.GuestService/Bridge",
  "/needy.v1.GuestService/TerminalTask",
  "/needy.v1.GuestService/FetchLayer",
  "/needy.v1.RunService/Decide",
  "/needy.v1.ProcessService/Exec",
  "/needy.v1.HealthService/Check",
];

/// Decides who a request is from.
#[async_trait::async_trait]
pub trait Authenticator: Send + Sync + 'static {
  /// The caller of `method` that presents the bearer `token` (empty
  /// without one) and optional `run_id` (`x-needy-run-id`), or `None` for
  /// invalid credentials. Backend lookup failures and authorization denials
  /// return an error independently of credential rejection.
  async fn authenticate(&self, method: &str, token: &str, run_id: Option<&str>) -> crate::control::Result<Option<Caller>>;
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
/// credential on [`RUNNER_METHODS`] (or mapped to `Caller::Actor` on
/// [`ACTOR_METHODS`] via the Node Authorizer when `x-needy-run-id` names an
/// active run bound to that runner), and a client for a paired client's token.
/// A token shaped like a runner credential or a client token that nobody holds
/// is refused even without a control token, so a removed runner or revoked
/// client learns that it was.
#[derive(Clone)]
pub struct Credentials {
  pub allow_anonymous: bool,
  pub listener: Caller,
  pub token: Token,
  pub runs: Option<Arc<dyn Runs>>,
  pub runners: Option<Arc<dyn Runners>>,
  pub clients: Option<Arc<dyn Clients>>,
}

#[async_trait::async_trait]
impl Authenticator for Credentials {
  async fn authenticate(&self, method: &str, token: &str, run_id: Option<&str>) -> crate::control::Result<Option<Caller>> {
    if PUBLIC_METHODS.contains(&method) {
      return Ok(Some(self.listener.clone()));
    }
    if (self.runners.is_some() && method == JOIN) || (self.clients.is_some() && method == PAIR) {
      return Ok(Some(self.listener.clone()));
    }
    if self.token.0.is_some() && self.token.accepts(token) {
      return Ok(Some(self.listener.clone()));
    }
    if let Some(runners) = &self.runners
      && !token.is_empty()
    {
      match runners.credential(token).await? {
        Credential::Runner(name) => {
          if let Some(run_id) = run_id {
            let authorized = match &self.runs {
              Some(runs) => runs.authorize_actor_runner(&name, run_id).await?,
              None => false,
            };
            if authorized {
              return Ok(Some(Caller::Actor {
                run: run_id.to_string(),
              }));
            }
            return Err(crate::control::ControlError::role_denied(format!("runner {name} is not authorized to act for run {run_id}")));
          }
          return Ok(RUNNER_METHODS.contains(&method).then_some(Caller::Runner { name }));
        }
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
            tonic::Status::from(crate::control::ControlError::role_denied("management socket admits only its owner and root")).into_http(),
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
          tonic::Status::from(
            crate::control::ControlError::Unauthenticated("missing or invalid bearer token".into())
              .with_diagnostic("auth.credential_rejected", []),
          )
          .into_http(),
        );
      }
      let run_id = if request.headers().contains_key(RUN_ID_HEADER) {
        let mut values = request.headers().get_all(RUN_ID_HEADER).iter();
        let valid = match (values.next(), values.next()) {
          (Some(v), None) => v.to_str().ok().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string),
          _ => None,
        };
        let Some(id) = valid else {
          return Ok(
            tonic::Status::from(
              crate::control::ControlError::Unauthenticated("invalid x-needy-run-id header".into())
                .with_diagnostic("auth.credential_rejected", []),
            )
            .into_http(),
          );
        };
        Some(id)
      } else {
        None
      };
      let method = request.uri().path();
      match auth.authenticate(method, &token, run_id.as_deref()).await {
        Ok(Some(caller)) => {
          if matches!(caller, Caller::Actor { .. }) && !ACTOR_METHODS.contains(&method) {
            return Ok(tonic::Status::from(crate::control::ControlError::role_denied("actors cannot call this method")).into_http());
          }
          request.extensions_mut().insert(caller);
          inner.call(request).await
        }
        Err(error @ (crate::control::ControlError::Denied { .. } | crate::control::ControlError::Diagnostic { .. })) => {
          Ok(tonic::Status::from(error).into_http())
        }
        Err(error) => Ok(tonic::Status::from(error.with_diagnostic("auth.backend_unavailable", [])).into_http()),
        Ok(None) => Ok(
          tonic::Status::from(
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
