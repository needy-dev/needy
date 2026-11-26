//! How the gateway reaches an actor's upstreams: directly from its own host,
//! or through an upstream SOCKS5 proxy the operator names
//! (`--gateway-upstream`), so that each actor can leave through a proxy of
//! its own (one actor runner Pod per actor on Kubernetes). See
//! `docs/ai/references/architecture/2026-11-26-runner-gateway-design.md`,
//! "Upstream proxy".
//!
//! The allowlist and chaos decide before any upstream is dialed; this module
//! only carries an allowed connection out. A configured proxy is never
//! bypassed: when it cannot be reached or refuses, the connection fails, and
//! the gateway answers 502 (fail closed).
//!
//! The SOCKS5 client is `tokio-socks` 0.5.3
//! (<https://docs.rs/tokio-socks/0.5.3>, MIT, the client `reqwest`'s
//! `socks` feature uses): CONNECT with a domain-name or IP target, and
//! username/password authentication.
// Reference: RFC 1928 (SOCKS Protocol Version 5), https://www.rfc-editor.org/rfc/rfc1928
// Reference: RFC 1929 (Username/Password Authentication for SOCKS V5), https://www.rfc-editor.org/rfc/rfc1929

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use tokio::io::AsyncReadExt as _;
use tokio::net::TcpStream;
use tokio_socks::IntoTargetAddr as _;
use tokio_socks::tcp::Socks5Stream;

use super::egress::{EgressError, EgressPolicy, resolve_and_connect, resolve_validated, strip_brackets};

/// The port a SOCKS proxy URL without one names.
// Source: RFC 1928 section 3, "The SOCKS service is conventionally located
// on TCP port 1080", https://www.rfc-editor.org/rfc/rfc1928#section-3
const SOCKS_PORT: u16 = 1080;

/// The longest username and password RFC 1929 carries (one length octet
/// each, at least 1).
// Source: RFC 1929 section 2, ULEN and PLEN, https://www.rfc-editor.org/rfc/rfc1929#section-2
const CREDENTIAL_MAX: usize = 255;

/// How the gateway reaches an upstream: the one seam every connection it
/// opens for the actor goes through, a tunnel's and a forwarded request's.
/// An operator's service (`actor.network.services`) is always dialed
/// [`Upstream::Direct`]: its URL names an endpoint the gateway's host
/// reaches (in-cluster on Kubernetes), not the internet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Upstream {
  /// Resolves and connects from the gateway's own host, after
  /// [`EgressPolicy`] validated the resolved addresses.
  #[default]
  Direct,
  /// Through a SOCKS5 proxy.
  Socks5(Socks5),
  // TODO(gateway-upstream): an HTTP CONNECT upstream proxy (`http://`,
  // `https://`) was considered and left out: the owner asked for SOCKS
  // (2026-11-26), and an HTTP proxy would need its own credential and TLS
  // handling. Add it when an orchestrator needs one.
}

/// A SOCKS5 proxy the gateway connects through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Socks5 {
  /// Who resolves a target's host name.
  names: Names,
  /// The proxy's host, unbracketed, resolved on the gateway's host.
  host: String,
  port: u16,
  /// A file of `USERNAME:PASSWORD`, read again for each connection, so a
  /// rotated Secret applies to the next one. The credentials themselves are
  /// never held here, so neither `Debug` nor a log line can show them.
  credentials: Option<PathBuf>,
}

/// Who resolves a target's host name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Names {
  /// `socks5h://`: the name goes to the proxy (address type 3, DOMAINNAME,
  /// RFC 1928 section 5); the gateway's host does not resolve it.
  Proxy,
  /// `socks5://`: the gateway resolves the name, validates every address
  /// against [`EgressPolicy`] and asks the proxy for an address.
  Gateway,
}

/// Why an upstream is invalid or failed. Never carries the credentials.
#[derive(Debug, thiserror::Error)]
pub enum UpstreamError {
  /// The operator's URL. The URL itself is not repeated: it may carry
  /// credentials the operator should not have put in it.
  #[error("invalid gateway upstream URL: {0}")]
  Url(&'static str),
  #[error("upstream proxy credentials file {path}: {reason}")]
  Credentials { path: PathBuf, reason: String },
  #[error("upstream proxy {proxy} is unreachable: {source}")]
  Unreachable {
    proxy: String,
    source: std::io::Error,
  },
  #[error("upstream proxy {proxy}: {source}")]
  Proxy {
    proxy: String,
    source: tokio_socks::Error,
  },
}

impl Upstream {
  /// The upstream `url` names: `socks5h://HOST[:PORT]` (the proxy resolves
  /// names) or `socks5://HOST[:PORT]` (the gateway does), port 1080 by
  /// default, authenticating with the `USERNAME:PASSWORD` in `credentials`
  /// when given. Credentials in the URL are refused: they would sit in argv
  /// and process listings.
  pub fn socks5(url: &str, credentials: Option<PathBuf>) -> Result<Self, UpstreamError> {
    let uri: hyper::Uri = url.parse().map_err(|_| UpstreamError::Url("not a URL"))?;
    let names = match uri.scheme_str() {
      Some("socks5h") => Names::Proxy,
      Some("socks5") => Names::Gateway,
      _ => return Err(UpstreamError::Url("the scheme must be socks5h:// or socks5://")),
    };
    let authority = uri.authority().ok_or(UpstreamError::Url("no proxy host"))?;
    if authority.as_str().contains('@') {
      return Err(UpstreamError::Url("credentials in the URL are refused; name a credentials file instead"));
    }
    if !matches!(uri.path(), "" | "/") || uri.query().is_some() {
      return Err(UpstreamError::Url("a proxy URL has no path or query"));
    }
    let host = strip_brackets(authority.host());
    if host.is_empty() {
      return Err(UpstreamError::Url("no proxy host"));
    }
    Ok(Self::Socks5(Socks5 {
      names,
      host: host.to_string(),
      port: authority.port_u16().unwrap_or(SOCKS_PORT),
      credentials,
    }))
  }

  /// Checks what can be checked before a connection: the credentials file
  /// reads and holds credentials. The proxy's reachability is not checked;
  /// each connection finds out, and fails closed.
  pub async fn check(&self) -> Result<(), UpstreamError> {
    match self {
      Self::Direct => Ok(()),
      Self::Socks5(proxy) => proxy.credentials().await.map(drop),
    }
  }

  /// What a trace records of this upstream on each connection through it
  /// (`via`): the proxy's URL, without credentials; none when direct.
  pub fn via(&self) -> Option<String> {
    match self {
      Self::Direct => None,
      Self::Socks5(proxy) => Some(proxy.to_string()),
    }
  }

  /// A connection to `(host, port)`, validated against `policy` when
  /// given (none for an operator's service).
  pub(super) async fn connect(&self, host: &str, port: u16, policy: Option<&EgressPolicy>) -> Result<TcpStream, EgressError> {
    match self {
      Self::Direct => resolve_and_connect(host, port, policy).await,
      Self::Socks5(proxy) => proxy.connect(host, port, policy).await,
    }
  }
}

impl fmt::Display for Socks5 {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let scheme = match self.names {
      Names::Proxy => "socks5h",
      Names::Gateway => "socks5",
    };
    if self.host.contains(':') {
      write!(f, "{scheme}://[{}]:{}", self.host, self.port)
    } else {
      write!(f, "{scheme}://{}:{}", self.host, self.port)
    }
  }
}

/// A username and password read from the credentials file. No `Debug`, so
/// it cannot be logged by accident.
struct Credentials {
  username: String,
  password: String,
}

impl Socks5 {
  async fn connect(&self, host: &str, port: u16, policy: Option<&EgressPolicy>) -> Result<TcpStream, EgressError> {
    let host = strip_brackets(host);
    // The file is read before anything is dialed: a broken one fails the
    // connection rather than trying the proxy without credentials.
    let credentials = self.credentials().await?;
    // NOTICE: with `socks5h` the proxy resolves a name, so `EgressPolicy`
    // (loopback, link-local, ULA, `--gateway-block-cidr`) cannot check the
    // address it reaches: blocking private and metadata ranges becomes the
    // proxy's job and the NetworkPolicy's (rc integration guide, section 8).
    // An IP literal needs no resolution and is still checked here, and
    // `socks5` resolves and checks every name here, as `Direct` does.
    let targets: Vec<Target<'_>> = if self.names == Names::Gateway || host.parse::<IpAddr>().is_ok() {
      resolve_validated(host, port, policy).await?.into_iter().map(Target::Address).collect()
    } else {
      vec![Target::Name(host, port)]
    };
    // Each address in turn, as a direct connection tries them; the last
    // failure is the one reported.
    let mut failed = None;
    for target in targets {
      match self.dial(target, credentials.as_ref()).await {
        Ok(stream) => return Ok(stream),
        Err(e) => failed = Some(e),
      }
    }
    Err(EgressError::Upstream(failed.unwrap_or(UpstreamError::Unreachable {
      proxy: self.to_string(),
      source: std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, format!("no addresses for {host}:{port}")),
    })))
  }

  /// One CONNECT through the proxy. The stream after a successful reply is
  /// the plain TCP connection to the target (`tokio-socks` reads exactly
  /// the reply), so it is returned as one.
  async fn dial(&self, target: Target<'_>, credentials: Option<&Credentials>) -> Result<TcpStream, UpstreamError> {
    let unreachable = |source| UpstreamError::Unreachable {
      proxy: self.to_string(),
      source,
    };
    let failed = |source| UpstreamError::Proxy {
      proxy: self.to_string(),
      source,
    };
    // NOTICE: the proxy is resolved and connected here, not by
    // `tokio-socks`, whose `ToProxyAddrs` for a host name resolves with the
    // blocking `std::net::ToSocketAddrs` (`src/lib.rs`, `impl ToProxyAddrs
    // for (&str, u16)`, v0.5.3). The proxy's own address is the operator's
    // choice (often in-cluster), so `EgressPolicy` does not apply to it.
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((self.host.as_str(), self.port)).await.map_err(unreachable)?.collect();
    let socket = TcpStream::connect(&addresses[..]).await.map_err(unreachable)?;
    let target = match target {
      Target::Name(host, port) => (host, port).into_target_addr(),
      Target::Address(address) => address.into_target_addr(),
    }
    .map_err(failed)?;
    // TODO(gateway-upstream): no handshake timeout, as a direct connection
    // has no connect timeout; a stalled proxy holds the actor's request
    // until the actor gives up or the run ends. Add one with a connect
    // timeout for both when a program is seen to depend on it.
    let connected = match credentials {
      Some(credentials) => {
        Socks5Stream::connect_with_password_and_socket(socket, target, &credentials.username, &credentials.password).await
      }
      None => Socks5Stream::connect_with_socket(socket, target).await,
    };
    connected.map(Socks5Stream::into_inner).map_err(failed)
  }

  /// The credentials in the file, if one is named: read now, so that a
  /// rotated Secret applies from the next connection.
  async fn credentials(&self) -> Result<Option<Credentials>, UpstreamError> {
    let Some(path) = &self.credentials else {
      return Ok(None);
    };
    read_credentials(path).await.map(Some).map_err(|reason| UpstreamError::Credentials {
      path: path.clone(),
      reason,
    })
  }
}

/// What a CONNECT asks the proxy for.
enum Target<'a> {
  /// A name the proxy resolves.
  Name(&'a str, u16),
  /// An address the gateway resolved and validated.
  Address(SocketAddr),
}

/// Reads `USERNAME:PASSWORD` (one line; the username has no `:`) from
/// `path`, refusing a file other users can access. The reason never
/// contains the file's text.
// NOTICE: a Kubernetes Secret volume is the expected source. Its files are
// symlinks into `..data/`, which the kubelet swaps atomically on rotation,
// so the path is followed (no `O_NOFOLLOW`, unlike the CLI's
// `configuration::read_secret`); they belong to root, so the owner is not
// checked; and a non-root container reads them through `fsGroup`, so group
// read is allowed. Only access by others is refused, which rejects the
// default `defaultMode: 0644`; the rc integration guide sets `0440`.
// Source: https://github.com/kubernetes/kubernetes/blob/v1.31.0/pkg/volume/util/atomic_writer.go
// (the `..data` symlink swap, described in `AtomicWriter.Write`'s comment;
// no line anchor, retrieved 2026-11-26).
// Reference: https://kubernetes.io/docs/concepts/configuration/secret/#using-secrets-as-files-from-a-pod
// (`defaultMode`, default 0644; unversioned page, retrieved 2026-11-26).
async fn read_credentials(path: &Path) -> Result<Credentials, String> {
  let mut file = tokio::fs::File::open(path).await.map_err(|e| format!("cannot open it: {e}"))?;
  let metadata = file.metadata().await.map_err(|e| format!("cannot inspect it: {e}"))?;
  if !metadata.is_file() {
    return Err("not a regular file".into());
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = metadata.permissions().mode();
    if mode & 0o007 != 0 {
      return Err(format!("other users can access it (mode {:o}); make it 0600, or 0440 with the Pod's fsGroup", mode & 0o777));
    }
  }
  let mut text = String::new();
  file.read_to_string(&mut text).await.map_err(|e| format!("cannot read it: {e}"))?;
  parse_credentials(&text)
}

/// `USERNAME:PASSWORD` on one line, a trailing newline allowed.
fn parse_credentials(text: &str) -> Result<Credentials, String> {
  let line = text.strip_suffix('\n').map(|line| line.strip_suffix('\r').unwrap_or(line)).unwrap_or(text);
  if line.contains(['\r', '\n', '\0']) {
    return Err("it must hold one line, USERNAME:PASSWORD".into());
  }
  let Some((username, password)) = line.split_once(':') else {
    return Err("it must hold USERNAME:PASSWORD".into());
  };
  if username.is_empty() || password.is_empty() {
    return Err("the username and the password must not be empty".into());
  }
  if username.len() > CREDENTIAL_MAX || password.len() > CREDENTIAL_MAX {
    return Err(format!("the username and the password are at most {CREDENTIAL_MAX} bytes each"));
  }
  Ok(Credentials {
    username: username.to_string(),
    password: password.to_string(),
  })
}

#[cfg(all(test, unix))]
#[path = "upstream_test.rs"]
mod tests;
