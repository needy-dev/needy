//! Resolve-then-validate egress IP filtering for the actor gateway.
//!
//! Every non-service outbound connection resolves DNS once, validates every
//! returned [`SocketAddr`] against [`EgressPolicy`], and connects directly to
//! the validated [`SocketAddr`] slice so DNS rebinding cannot redirect a
//! second lookup to loopback, link-local, metadata, or private endpoints.

use std::fmt;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::task::{Context, Poll};

use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;

/// An IPv4 or IPv6 CIDR block (e.g. `10.0.0.0/8`, `fd00::/8`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpCidr {
  network: IpAddr,
  prefix_len: u8,
}

impl IpCidr {
  /// Constructs a CIDR block, masking host bits in `network`.
  pub fn new(network: IpAddr, prefix_len: u8) -> Result<Self, String> {
    match network {
      IpAddr::V4(v4) => {
        if prefix_len > 32 {
          return Err(format!("IPv4 prefix length {prefix_len} exceeds 32"));
        }
        let mask = ipv4_mask(prefix_len);
        Ok(Self {
          network: IpAddr::V4(Ipv4Addr::from(u32::from(v4) & mask)),
          prefix_len,
        })
      }
      IpAddr::V6(v6) => {
        if prefix_len > 128 {
          return Err(format!("IPv6 prefix length {prefix_len} exceeds 128"));
        }
        let mask = ipv6_mask(prefix_len);
        Ok(Self {
          network: IpAddr::V6(Ipv6Addr::from(u128::from(v6) & mask)),
          prefix_len,
        })
      }
    }
  }

  /// Whether `ip` (or its canonical form, for IPv4-mapped IPv6 addresses such
  /// as `::ffff:10.0.0.1`) falls within this CIDR block.
  pub fn contains(&self, ip: IpAddr) -> bool {
    self.contains_exact(ip) || self.contains_exact(ip.to_canonical())
  }

  fn contains_exact(&self, ip: IpAddr) -> bool {
    match (self.network, ip) {
      (IpAddr::V4(net), IpAddr::V4(addr)) => {
        let mask = ipv4_mask(self.prefix_len);
        (u32::from(addr) & mask) == u32::from(net)
      }
      (IpAddr::V6(net), IpAddr::V6(addr)) => {
        let mask = ipv6_mask(self.prefix_len);
        (u128::from(addr) & mask) == u128::from(net)
      }
      _ => false,
    }
  }
}

impl fmt::Display for IpCidr {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}/{}", self.network, self.prefix_len)
  }
}

impl FromStr for IpCidr {
  type Err = String;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    let (addr_str, prefix_str) = s.split_once('/').ok_or_else(|| format!("invalid CIDR {s:?}: missing '/'"))?;
    let addr: IpAddr = addr_str.parse().map_err(|e| format!("invalid CIDR address {addr_str:?}: {e}"))?;
    let prefix_len: u8 = prefix_str.parse().map_err(|e| format!("invalid CIDR prefix {prefix_str:?}: {e}"))?;
    Self::new(addr, prefix_len)
  }
}

fn ipv4_mask(prefix_len: u8) -> u32 {
  if prefix_len == 0 {
    0
  } else {
    !0u32 << (32 - prefix_len)
  }
}

fn ipv6_mask(prefix_len: u8) -> u128 {
  if prefix_len == 0 {
    0
  } else {
    !0u128 << (128 - prefix_len)
  }
}

/// Why an IP address was rejected by [`EgressPolicy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockedReason {
  /// Loopback (`127.0.0.0/8`, `::1`).
  Loopback,
  /// Link-local (`169.254.0.0/16`, `fe80::/10`), including cloud IMDSv1/v2.
  LinkLocal,
  /// IPv6 Unique Local Address (`fc00::/7`), including AWS IPv6 IMDS (`fd00:ec2::254`).
  Ula,
  /// Unspecified / "this network" (`0.0.0.0/8`, `::`).
  Unspecified,
  /// Matched an operator-configured blocked CIDR.
  Cidr(IpCidr),
}

impl fmt::Display for BlockedReason {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Loopback => f.write_str("loopback"),
      Self::LinkLocal => f.write_str("link-local"),
      Self::Ula => f.write_str("unique-local"),
      Self::Unspecified => f.write_str("unspecified"),
      Self::Cidr(cidr) => write!(f, "blocked CIDR {cidr}"),
    }
  }
}

/// Policy governing which resolved IP addresses the gateway may connect to for
/// non-service (actor-chosen) targets.
#[derive(Debug, Clone)]
pub struct EgressPolicy {
  /// Block loopback (`127.0.0.0/8`, `::1`), link-local (`169.254.0.0/16`,
  /// `fe80::/10`), ULA (`fc00::/7`, including `fd00:ec2::254`), unspecified
  /// (`0.0.0.0/8`, `::`), and IPv4-mapped IPv6 equivalents (`::ffff:0:0/96`).
  pub block_local_and_metadata: bool,
  /// Additional operator-configured CIDRs to block (e.g. RFC1918 or cluster
  /// Pod/Service CIDRs).
  pub blocked_cidrs: Vec<IpCidr>,
}

impl Default for EgressPolicy {
  fn default() -> Self {
    Self {
      block_local_and_metadata: true,
      blocked_cidrs: Vec::new(),
    }
  }
}

impl EgressPolicy {
  /// Disables all egress IP blocking. Intended for tests that forward directly
  /// to a local loopback test server without configuring a service.
  pub fn allow_all() -> Self {
    Self {
      block_local_and_metadata: false,
      blocked_cidrs: Vec::new(),
    }
  }

  /// Adds operator-configured CIDRs to the blocklist.
  pub fn with_blocked_cidrs(mut self, cidrs: Vec<IpCidr>) -> Self {
    self.blocked_cidrs = cidrs;
    self
  }

  /// Validates a single resolved IP address (and its canonical IPv4 form when
  /// IPv4-mapped IPv6).
  pub fn check(&self, ip: IpAddr) -> Result<(), BlockedReason> {
    let canonical = ip.to_canonical();
    if self.block_local_and_metadata {
      match canonical {
        IpAddr::V4(v4) => {
          if v4.is_loopback() {
            return Err(BlockedReason::Loopback);
          }
          if v4.is_link_local() {
            return Err(BlockedReason::LinkLocal);
          }
          // Block all of 0.0.0.0/8 (RFC 1122 "this network"), which Linux
          // routes to loopback in addition to 0.0.0.0 itself.
          if v4.is_unspecified() || v4.octets()[0] == 0 {
            return Err(BlockedReason::Unspecified);
          }
        }
        IpAddr::V6(v6) => {
          if v6.is_loopback() {
            return Err(BlockedReason::Loopback);
          }
          if v6.is_unspecified() {
            return Err(BlockedReason::Unspecified);
          }
          if v6.is_unicast_link_local() {
            return Err(BlockedReason::LinkLocal);
          }
          // RFC 4193 fc00::/7 Unique Local Addresses, including AWS IPv6 IMDS
          // (fd00:ec2::254).
          if (v6.segments()[0] & 0xfe00) == 0xfc00 {
            return Err(BlockedReason::Ula);
          }
        }
      }
    }
    for cidr in &self.blocked_cidrs {
      if cidr.contains(ip) {
        return Err(BlockedReason::Cidr(*cidr));
      }
    }
    Ok(())
  }
}

/// Error returned when resolving, validating, or connecting to an upstream.
#[derive(Debug, thiserror::Error)]
pub(super) enum EgressError {
  #[error("target {host} resolved to blocked address {ip} ({reason})")]
  Blocked {
    host: String,
    ip: IpAddr,
    reason: BlockedReason,
  },
  #[error("invalid upstream URI: {0}")]
  InvalidUri(String),
  #[error(transparent)]
  Io(#[from] std::io::Error),
}

/// Strips surrounding `[` and `]` from an IPv6 literal host returned by
/// `http::Uri::host()` or `http::uri::Authority::host()`.
pub(super) fn strip_brackets(host: &str) -> &str {
  host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host)
}

/// Resolves `(host, port)` once, validates every returned [`SocketAddr`]
/// against `policy` (if present), and connects directly to the validated
/// addresses so no second DNS lookup can occur.
pub(super) async fn resolve_and_connect(host: &str, port: u16, policy: Option<&EgressPolicy>) -> Result<TcpStream, EgressError> {
  let host = strip_brackets(host);
  let addrs: Vec<SocketAddr> = if let Ok(ip) = host.parse::<IpAddr>() {
    vec![SocketAddr::new(ip, port)]
  } else {
    tokio::net::lookup_host((host, port)).await?.collect()
  };
  if addrs.is_empty() {
    return Err(std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, format!("no addresses resolved for {host}:{port}")).into());
  }
  if let Some(policy) = policy {
    for addr in &addrs {
      if let Err(reason) = policy.check(addr.ip()) {
        return Err(EgressError::Blocked {
          host: host.to_string(),
          ip: addr.ip(),
          reason,
        });
      }
    }
  }
  Ok(TcpStream::connect(&addrs[..]).await?)
}

/// Walks an error's source chain to find an [`EgressError::Blocked`].
pub(super) fn blocked_in_chain<'a>(err: &'a (dyn std::error::Error + 'static)) -> Option<&'a EgressError> {
  let mut current: Option<&(dyn std::error::Error + 'static)> = Some(err);
  while let Some(e) = current {
    if let Some(egress @ EgressError::Blocked { .. }) = e.downcast_ref::<EgressError>() {
      return Some(egress);
    }
    current = e.source();
  }
  None
}

/// A `hyper` HTTP connector that resolves DNS once, validates every resolved IP
/// against an optional [`EgressPolicy`], and dials the validated [`SocketAddr`]
/// directly. Wrapped by `hyper_rustls::HttpsConnector` so TLS SNI still uses
/// the original URI host.
#[derive(Clone)]
pub(super) struct ValidatedConnector {
  policy: Option<Arc<EgressPolicy>>,
}

impl ValidatedConnector {
  pub(super) fn new(policy: Option<Arc<EgressPolicy>>) -> Self {
    Self { policy }
  }
}

impl tower::Service<hyper::Uri> for ValidatedConnector {
  type Response = TokioIo<TcpStream>;
  type Error = EgressError;
  type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

  fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
    Poll::Ready(Ok(()))
  }

  fn call(&mut self, uri: hyper::Uri) -> Self::Future {
    let policy = self.policy.clone();
    Box::pin(async move {
      let Some(raw_host) = uri.host() else {
        return Err(EgressError::InvalidUri(format!("missing host in {uri}")));
      };
      let host = strip_brackets(raw_host).to_string();
      let default_port = if uri.scheme_str() == Some("https") {
        443
      } else {
        80
      };
      let port = uri.port_u16().unwrap_or(default_port);
      let stream = resolve_and_connect(&host, port, policy.as_deref()).await?;
      Ok(TokioIo::new(stream))
    })
  }
}
