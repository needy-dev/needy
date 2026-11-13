//! A server: the services one [`Caller`] reaches, bound to every listener
//! before any of them serves, so a listener that cannot bind fails at once
//! with its address.

#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use needy_api_proto::v1::client_service_server::ClientServiceServer;
use needy_api_proto::v1::guest_service_server::GuestServiceServer;
use needy_api_proto::v1::health_service_server::HealthServiceServer;
use needy_api_proto::v1::process_service_server::{ProcessService, ProcessServiceServer};
use needy_api_proto::v1::run_service_server::RunServiceServer;
use needy_api_proto::v1::runner_service_server::RunnerServiceServer;
use tokio_util::sync::CancellationToken;
use tonic::service::Routes;

use crate::control::{Caller, Clients, Guest, Processes, Runners, Runs};
use crate::middleware::authentication::{AuthLayer, Credentials, Token};
use crate::protocol::grpc::v1::{client, guest, health::Health, process, run, runner};

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
  /// A server answers on the Unix socket already; its file is left alone.
  #[error("another server is listening on {0}")]
  InUse(Listen),
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
  /// The runner registry, which also authenticates runners' credentials.
  runners: Option<Arc<dyn Runners>>,
  /// The client registry, which also authenticates clients' tokens.
  clients: Option<Arc<dyn Clients>>,
  /// The group the Unix sockets this server binds are given to, mode 0660.
  socket_group: Option<u32>,
  management: Option<(PathBuf, u32)>,
}

impl Server {
  /// A server with `HealthService`, whose other services see `caller`.
  pub fn new(health: Health, caller: Caller) -> Self {
    Self {
      routes: Routes::new(HealthServiceServer::new(health)),
      caller,
      token: None,
      runners: None,
      clients: None,
      socket_group: None,
      management: None,
    }
  }

  /// Gives every Unix socket this server binds to group `gid`, mode 0660,
  /// instead of mode 0600: for a server whose client is another user in
  /// that group, such as a container executor's daemon of another user
  /// (`needy daemon --socket-group`). The server's user must be a member of
  /// the group.
  /// Designates the private owner/root management entry. Other control
  /// listeners require explicit registered credentials, including loopback.
  pub fn management_socket(mut self, path: PathBuf, owner_uid: u32) -> Self {
    self.management = Some((path, owner_uid));
    self
  }

  pub fn socket_group(mut self, gid: Option<u32>) -> Self {
    self.socket_group = gid;
    self
  }

  /// A static incoming token for standalone services and explicit legacy
  /// callers. Registered client/runner credentials retain their own identity.
  /// A management socket grants operator admission independently of this token.
  pub fn token(mut self, token: Option<String>) -> Self {
    self.token = token;
    self
  }

  pub fn runs<S: Runs>(mut self, runs: Arc<S>) -> Self {
    let caller = self.caller.clone();
    self.routes = self.routes.add_service(RunServiceServer::new(run::Adapter { runs, caller }));
    self
  }

  /// `RunnerService`. Requests that present a runner's credential are then
  /// from that runner, and `Join` needs no bearer token.
  pub fn runners<R: Runners>(mut self, runners: Arc<R>) -> Self {
    let caller = self.caller.clone();
    self.routes = self.routes.add_service(RunnerServiceServer::new(runner::Adapter {
      runners: runners.clone(),
      caller,
    }));
    self.runners = Some(runners);
    self
  }

  /// `ClientService`. Requests that present a client's token are then
  /// from that client, and `Pair` needs no bearer token.
  pub fn clients<C: Clients>(mut self, clients: Arc<C>) -> Self {
    let caller = self.caller.clone();
    self.routes = self.routes.add_service(ClientServiceServer::new(client::Adapter {
      clients: clients.clone(),
      caller,
    }));
    self.clients = Some(clients);
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
  /// earlier server that no longer listens is replaced; one where a server
  /// still answers is [`Error::InUse`]. The socket files this server binds
  /// are removed when it stops serving, or when [`Bound`] is dropped
  /// without serving. A server with a designated management socket requires
  /// credentials on its other listeners. Otherwise a tokenless TCP listener
  /// off loopback is refused; loopback starts with a warning. A TCP
  /// listener on port 0 gets a free port; [`Bound::addresses`] tells which.
  pub async fn bind(self, listen: &[Listen]) -> Result<Bound, Error> {
    let mut listeners = Vec::with_capacity(listen.len());
    #[cfg(unix)]
    let mut sockets = Vec::new();
    for listen in listen {
      let management = self.management.as_ref().is_some_and(|(path, _)| *listen == Listen::Unix(path.clone()));
      let listener = Listener::bind(listen, if management { None } else { self.socket_group }).await?;
      #[cfg(unix)]
      if let (Listener::Unix(_), Listen::Unix(path)) = (&listener, listen) {
        // Held from here, so a later listener that fails to bind removes
        // this one's file too.
        sockets.push(SocketFile::bound(path).map_err(|source| Error::Bind {
          listen: listen.clone(),
          source,
        })?);
      }
      if self.token.is_none() && self.management.is_none() {
        listener.check_unauthenticated(listen)?;
      }
      listeners.push((listener.address(listen), listener));
    }
    Ok(Bound {
      routes: self.routes,
      credentials: Credentials {
        allow_anonymous: true,
        listener: self.caller,
        token: Token(self.token),
        runners: self.runners,
        clients: self.clients,
      },
      listeners,
      management: self.management,
      #[cfg(unix)]
      sockets,
    })
  }
}

/// A Unix socket file a server bound, removed when this is dropped: when
/// the server stops serving, and also when its `serve` future is dropped
/// where it was, as a forced exit does.
#[cfg(unix)]
struct SocketFile {
  path: PathBuf,
  /// The file's device and inode at bind, so a socket another server bound
  /// at the same path since is not removed.
  id: (u64, u64),
}

#[cfg(unix)]
impl SocketFile {
  fn bound(path: &Path) -> std::io::Result<Self> {
    Ok(Self {
      path: path.to_path_buf(),
      id: Self::id(path)?,
    })
  }

  fn id(path: &Path) -> std::io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(path)?;
    Ok((metadata.dev(), metadata.ino()))
  }
}

#[cfg(unix)]
impl Drop for SocketFile {
  fn drop(&mut self) {
    // NOTICE: between the check and the removal another server could replace
    // the file. One that starts while this server still listens is refused
    // (`Error::InUse`), so only a start in the moment between this server's
    // listener closing and this drop can lose its socket.
    match Self::id(&self.path) {
      Ok(id) if id == self.id => {
        if let Err(e) = std::fs::remove_file(&self.path)
          && e.kind() != std::io::ErrorKind::NotFound
        {
          tracing::warn!("cannot remove {}: {e}", self.path.display());
        }
      }
      // Removed already, or replaced by another server's socket.
      _ => {}
    }
  }
}

/// Clears `path` for a new socket: a file where no server answers, such as
/// the socket of a server that was killed, is removed; a socket where one
/// answers is [`Error::InUse`].
#[cfg(unix)]
async fn clear_stale_socket(path: &Path, listen: &Listen) -> Result<(), Error> {
  match tokio::net::UnixStream::connect(path).await {
    Ok(_) => return Err(Error::InUse(listen.clone())),
    // NOTICE: a full accept backlog makes a non-blocking connect fail with
    // `EAGAIN` on Linux (`unix(7)`); a server still listens there.
    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Err(Error::InUse(listen.clone())),
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
    // Refused: nobody listens. Any other error, such as a file that is not
    // a socket, leaves the file to the removal below, as before.
    Err(_) => {}
  }
  if let Err(e) = std::fs::remove_file(path)
    && e.kind() != std::io::ErrorKind::NotFound
  {
    tracing::warn!("cannot remove {}: {e}", path.display());
  }
  Ok(())
}

/// Creates `dir` and its missing parents with mode 0700, whatever the
/// umask. Directories that exist already keep their mode: they may be shared
/// ones such as `/tmp`.
#[cfg(unix)]
pub fn create_private_dir(dir: &Path) -> std::io::Result<()> {
  use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
  let missing: Vec<&Path> = dir.ancestors().take_while(|d| !d.as_os_str().is_empty() && !d.exists()).collect();
  std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
  // The umask may have narrowed `mode`; the mode is set again so it is
  // exactly 0700.
  for created in missing {
    std::fs::set_permissions(created, std::fs::Permissions::from_mode(0o700))?;
  }
  Ok(())
}

enum Listener {
  #[cfg(unix)]
  Unix(tokio::net::UnixListener),
  Tcp(tokio::net::TcpListener),
}

impl Listener {
  async fn bind(listen: &Listen, group: Option<u32>) -> Result<Self, Error> {
    let failed = |source| Error::Bind {
      listen: listen.clone(),
      source,
    };
    match listen {
      #[cfg(unix)]
      Listen::Unix(path) => {
        if let Some(parent) = path.parent() {
          create_private_dir(parent).map_err(failed)?;
        }
        clear_stale_socket(path, listen).await?;
        // An over-long path is an `InvalidInput` error here, not a
        // truncated name.
        let listener = tokio::net::UnixListener::bind(path).map_err(failed)?;
        // NOTICE: `bind` creates the socket with the umask's mode; it is
        // narrowed right after. A directory this server created is 0700, so
        // nobody else can reach the socket in between.
        use std::os::unix::fs::PermissionsExt;
        let mode = match group {
          Some(gid) => {
            std::os::unix::fs::chown(path, None, Some(gid)).map_err(failed)?;
            0o660
          }
          None => 0o600,
        };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(failed)?;
        Ok(Self::Unix(listener))
      }
      #[cfg(not(unix))]
      Listen::Unix(_) => {
        // Unix sockets, and so their group, are unsupported here.
        let _ = group;
        Err(Error::Unsupported(listen.clone()))
      }
      Listen::Tcp(address) => Ok(Self::Tcp(tokio::net::TcpListener::bind(address).await.map_err(failed)?)),
    }
  }
}

impl Listener {
  /// Where this listener, bound for `listen`, listens: the port the system
  /// picked for port 0, and the address a host name resolved to.
  fn address(&self, listen: &Listen) -> Listen {
    match self {
      Self::Tcp(listener) => match listener.local_addr() {
        Ok(address) => Listen::Tcp(address.to_string()),
        // A bound socket has an address; `listen` is the next best name.
        Err(_) => listen.clone(),
      },
      #[cfg(unix)]
      Self::Unix(_) => listen.clone(),
    }
  }

  /// Refuses a TCP listener without a token unless only this machine can
  /// reach it. The bound address decides, so a host name is judged by what
  /// it resolved to.
  fn check_unauthenticated(&self, listen: &Listen) -> Result<(), Error> {
    let listener = match self {
      Self::Tcp(listener) => listener,
      // A Unix socket is guarded by its file permissions.
      #[cfg(unix)]
      Self::Unix(_) => return Ok(()),
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
  credentials: Credentials,
  listeners: Vec<(Listen, Listener)>,
  management: Option<(PathBuf, u32)>,
  /// The Unix socket files bound, removed when this server stops serving.
  #[cfg(unix)]
  sockets: Vec<SocketFile>,
}

impl Bound {
  /// The addresses this server listens on, in the order they were given.
  pub fn addresses(&self) -> Vec<Listen> {
    self.listeners.iter().map(|(listen, _)| listen.clone()).collect()
  }

  /// Serves every listener until `cancel`, then lets open calls finish for
  /// a grace period.
  pub async fn serve(self, cancel: CancellationToken) -> Result<(), Error> {
    // Dropped when this returns or is dropped, which removes the files.
    #[cfg(unix)]
    let _sockets = self.sockets;
    let credentials = self.credentials;
    let mut serving = tokio::task::JoinSet::new();
    for (listen, listener) in self.listeners {
      let mut credentials = credentials.clone();
      let owner_uid = self.management.as_ref().and_then(|(path, uid)| (listen == Listen::Unix(path.clone())).then_some(*uid));
      if self.management.is_some() {
        credentials.allow_anonymous = owner_uid.is_some();
      }
      // Local admission, rather than a legacy shared token, grants operator authority.
      if owner_uid.is_some() {
        credentials.token = Token(None);
      }
      let auth = AuthLayer::new(credentials).owner(owner_uid);
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
