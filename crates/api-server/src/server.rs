//! A server: the services one [`Caller`] reaches, bound to every listener
//! before any of them serves, so a listener that cannot bind fails at once
//! with its address.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use needy_api_proto::v1::guest_service_server::GuestServiceServer;
use needy_api_proto::v1::health_service_server::HealthServiceServer;
use needy_api_proto::v1::process_service_server::{ProcessService, ProcessServiceServer};
use needy_api_proto::v1::session_service_server::SessionServiceServer;
use tokio_util::sync::CancellationToken;
use tonic::service::Routes;

use crate::control::{Caller, Guest, Processes, Sessions};
use crate::middleware::authentication::{AuthLayer, Token};
use crate::protocol::grpc::v1::{guest, health::Health, process, session};

// NOTICE: once cancelled, a server stops accepting and lets open calls
// finish; an attached exec stream may never finish, so it gets 5 s.
const GRACE: Duration = Duration::from_secs(5);

/// Where to listen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listen {
  /// A Unix socket, including a Firecracker vsock's host-side socket.
  Unix(PathBuf),
  /// `HOST:PORT`.
  Tcp(String),
}

impl Listen {
  /// `unix:/path`, or `HOST:PORT`.
  pub fn parse(text: &str) -> Self {
    match text.strip_prefix("unix:") {
      Some(path) => Self::Unix(PathBuf::from(path)),
      None => Self::Tcp(text.to_string()),
    }
  }
}

impl std::fmt::Display for Listen {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Unix(path) => write!(f, "unix:{}", path.display()),
      Self::Tcp(address) => f.write_str(address),
    }
  }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("cannot listen on {listen}")]
  Bind {
    listen: Listen,
    #[source]
    source: std::io::Error,
  },
  /// A TCP listener reachable from other machines would serve every method
  /// to anyone; one without a token does not start.
  #[error("will not listen on {0} without a token: the address is not a loopback address")]
  Unauthenticated(Listen),
  #[error("unix sockets are not supported here: {0}")]
  Unsupported(Listen),
  #[error("serving {listen}")]
  Serve {
    listen: Listen,
    #[source]
    source: tonic::transport::Error,
  },
}

/// The services of one server, before it binds.
pub struct Server {
  routes: Routes,
  caller: Caller,
  token: Option<String>,
}

impl Server {
  /// A server with `HealthService`, whose other services see `caller`.
  pub fn new(health: Health, caller: Caller) -> Self {
    Self {
      routes: Routes::new(HealthServiceServer::new(health)),
      caller,
      token: None,
    }
  }

  /// The bearer token every method but health needs; any token is accepted
  /// when there is none.
  pub fn token(mut self, token: Option<String>) -> Self {
    self.token = token;
    self
  }

  pub fn sessions<S: Sessions>(mut self, sessions: Arc<S>) -> Self {
    let caller = self.caller.clone();
    self.routes = self.routes.add_service(SessionServiceServer::new(session::Adapter { sessions, caller }));
    self
  }

  pub fn processes<P: Processes>(self, processes: Arc<P>) -> Self {
    let caller = self.caller.clone();
    self.process_service(process::Adapter { processes, caller })
  }

  pub fn guest<G: Guest>(mut self, guest: Arc<G>) -> Self {
    let caller = self.caller.clone();
    self.routes = self.routes.add_service(GuestServiceServer::new(guest::Adapter { guest, caller }));
    self
  }

  /// A `ProcessService` that speaks the generated types itself, such as an
  /// executor's.
  pub fn process_service<P: ProcessService>(mut self, service: P) -> Self {
    // Exec streams carry whatever a process writes; output chunks are not
    // bounded by gRPC's default 4 MiB.
    let service = ProcessServiceServer::new(service).max_decoding_message_size(usize::MAX).max_encoding_message_size(usize::MAX);
    self.routes = self.routes.add_service(service);
    self
  }

  /// Binds every listener, or none: the first that cannot bind is the
  /// error. A Unix socket's directory is created, and a socket left by an
  /// earlier server is replaced. Without a token, a TCP listener on a
  /// non-loopback address is refused ([`Error::Unauthenticated`]); one on a
  /// loopback address, where tunnels end, starts with a warning.
  pub async fn bind(self, listen: &[Listen]) -> Result<Bound, Error> {
    let mut listeners = Vec::with_capacity(listen.len());
    for listen in listen {
      let listener = Listener::bind(listen).await?;
      if self.token.is_none() {
        listener.check_unauthenticated(listen)?;
      }
      listeners.push((listen.clone(), listener));
    }
    Ok(Bound {
      routes: self.routes,
      token: self.token,
      listeners,
    })
  }
}

enum Listener {
  #[cfg(unix)]
  Unix(tokio::net::UnixListener),
  Tcp(tokio::net::TcpListener),
}

impl Listener {
  async fn bind(listen: &Listen) -> Result<Self, Error> {
    let failed = |source| Error::Bind {
      listen: listen.clone(),
      source,
    };
    match listen {
      #[cfg(unix)]
      Listen::Unix(path) => {
        if let Some(parent) = path.parent() {
          std::fs::create_dir_all(parent).map_err(failed)?;
        }
        if let Err(e) = std::fs::remove_file(path)
          && e.kind() != std::io::ErrorKind::NotFound
        {
          tracing::warn!("cannot remove {}: {e}", path.display());
        }
        // An over-long path is an `InvalidInput` error here, not a
        // truncated name.
        Ok(Self::Unix(tokio::net::UnixListener::bind(path).map_err(failed)?))
      }
      #[cfg(not(unix))]
      Listen::Unix(_) => Err(Error::Unsupported(listen.clone())),
      Listen::Tcp(address) => Ok(Self::Tcp(tokio::net::TcpListener::bind(address).await.map_err(failed)?)),
    }
  }
}

impl Listener {
  /// Refuses a TCP listener without a token unless only this machine can
  /// reach it. The bound address decides, so a host name is judged by what
  /// it resolved to.
  fn check_unauthenticated(&self, listen: &Listen) -> Result<(), Error> {
    let Self::Tcp(listener) = self else {
      // A Unix socket is guarded by its file permissions.
      return Ok(());
    };
    let loopback = listener.local_addr().is_ok_and(|address| address.ip().to_canonical().is_loopback());
    if !loopback {
      return Err(Error::Unauthenticated(listen.clone()));
    }
    tracing::warn!("listening on {listen} without a token: every local user, and every tunnel that ends here, can use it");
    Ok(())
  }
}

/// A server bound to its listeners.
pub struct Bound {
  routes: Routes,
  token: Option<String>,
  listeners: Vec<(Listen, Listener)>,
}

impl Bound {
  /// Serves every listener until `cancel`, then lets open calls finish for
  /// a grace period.
  pub async fn serve(self, cancel: CancellationToken) -> Result<(), Error> {
    let auth = AuthLayer::new(Token(self.token));
    let mut serving = tokio::task::JoinSet::new();
    for (listen, listener) in self.listeners {
      let router = tonic::transport::Server::builder()
        .http2_keepalive_interval(Some(Duration::from_secs(20)))
        .tcp_nodelay(true)
        .layer(auth.clone())
        .add_routes(self.routes.clone());
      let stop = cancel.clone().cancelled_owned();
      serving.spawn(async move {
        let served = match listener {
          #[cfg(unix)]
          Listener::Unix(listener) => {
            router.serve_with_incoming_shutdown(tokio_stream::wrappers::UnixListenerStream::new(listener), stop).await
          }
          Listener::Tcp(listener) => {
            router.serve_with_incoming_shutdown(tokio_stream::wrappers::TcpListenerStream::new(listener), stop).await
          }
        };
        served.map_err(|source| Error::Serve { listen, source })
      });
    }
    let all = async {
      while let Some(joined) = serving.join_next().await {
        match joined {
          Ok(served) => served?,
          // A panic in a listener's task is a bug; the others keep serving.
          Err(e) => tracing::error!("a listener's task failed: {e}"),
        }
      }
      Ok(())
    };
    tokio::select! {
      served = all => served,
      () = async { cancel.cancelled().await; tokio::time::sleep(GRACE).await } => Ok(()),
    }
  }
}

#[cfg(test)]
#[path = "server_test.rs"]
mod tests;
