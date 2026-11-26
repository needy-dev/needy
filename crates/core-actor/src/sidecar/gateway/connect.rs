//! `CONNECT host:port` tunnels. The tunnel is opaque, so a fault cannot
//! answer with a status. A service's host (`NAME.needy`) tunnels to the
//! service's URL: some HTTP clients tunnel even plain HTTP through a proxy
//! (pi 1.5.2's `fetch` sent `CONNECT mock.needy:80` for `http://mock.needy`
//! on 2026-11-13), and the bytes in the tunnel are the plain HTTP the
//! service speaks.
// Source: https://github.com/nodejs/undici/blob/v6.21.3/lib/dispatcher/proxy-agent.js#L66-L90
// (undici 6's `ProxyAgent` opens every connection with CONNECT, `http:` too,
// and fails with "Proxy response (502) !== 200 when HTTP Tunneling", the
// error pi printed). undici 7 added `proxyTunnel` (default off for plain
// HTTP):
// https://github.com/nodejs/undici/blob/v7.16.0/docs/docs/api/ProxyAgent.md

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use needy_core::event::{Event, NetConnect, NetDeny, NetError};

use super::egress::{EgressError, strip_brackets};
use super::upstream::Upstream;
use super::{Body, Gateway, Reset, text};

impl Gateway {
  /// Opens a tunnel to the request's authority. The tunnel is a task the
  /// gateway owns; it ends with either side or the owner.
  pub(super) async fn connect(&self, request: Request<Incoming>) -> Result<Response<Body>, Reset> {
    let Some((host, port, authority)) = parse_connect_authority(request.uri().authority()) else {
      return Ok(text(StatusCode::BAD_REQUEST, "CONNECT needs host:port"));
    };
    let decision = self.decide(&host, None).await;
    if let Some(refusal) = Self::refusal(&decision, &host) {
      return refusal;
    }
    // A service's URL is dialed directly, as `Gateway::with_options` says.
    let (upstream_host, upstream_port, egress_policy, dial) = match self.policy.service(&host) {
      None => (host.clone(), port, Some(self.egress.as_ref()), &self.upstream),
      Some(url) => match service_authority(url) {
        Some((svc_host, svc_port)) => (svc_host, svc_port, None, &Upstream::Direct),
        None => return Ok(text(StatusCode::BAD_GATEWAY, "needy: the service's URL is not plain http://host[:port]")),
      },
    };
    let via = dial.via();
    let started = std::time::Instant::now();
    let connected = dial.connect(&upstream_host, upstream_port, egress_policy).await;
    self.trace.time("net.connect", started.elapsed());
    let mut upstream = match connected {
      Ok(stream) => stream,
      Err(EgressError::Blocked { ip, reason, .. }) => {
        let message = format!("needy: {host} resolved to blocked address {ip} ({reason})");
        self.record(Event::NetDeny(NetDeny {
          network: self.network,
          host,
          path: None,
        }));
        return Ok(text(StatusCode::FORBIDDEN, &message));
      }
      Err(e) => {
        let message = format!("needy: {e}");
        self.record(Event::NetError(NetError {
          network: self.network,
          host,
          path: None,
          error: e.to_string(),
          via,
        }));
        return Ok(text(StatusCode::BAD_GATEWAY, &message));
      }
    };
    self.record(Event::NetConnect(NetConnect {
      network: self.network,
      host,
      authority: authority.clone(),
      via,
    }));
    let cancel = self.cancel.clone();
    self.tasks.spawn(async move {
      let upgraded = match hyper::upgrade::on(request).await {
        Ok(upgraded) => upgraded,
        Err(e) => {
          tracing::debug!("tunnel to {authority}: the client did not upgrade: {e}");
          return;
        }
      };
      let mut client = TokioIo::new(upgraded);
      tokio::select! {
        result = tokio::io::copy_bidirectional(&mut client, &mut upstream) => {
          if let Err(e) = result {
            tracing::debug!("tunnel to {authority}: {e}");
          }
        }
        () = cancel.cancelled() => {}
      }
    });
    Ok(Response::new(Full::new(Bytes::new()).map_err(|never| match never {}).boxed()))
  }
}

/// Parses a `CONNECT` authority (`host:port` or `[ipv6]:port`) into its
/// unbracketed host, port, and original authority string.
pub(super) fn parse_connect_authority(authority: Option<&hyper::http::uri::Authority>) -> Option<(String, u16, String)> {
  let authority = authority?;
  let port = authority.port_u16()?;
  let host = strip_brackets(authority.host());
  if host.is_empty() {
    return None;
  }
  Some((host.to_string(), port, authority.to_string()))
}

/// The `(host, port)` a plain-HTTP service URL names (port 80 by default);
/// none for another scheme, which a tunnel of plain HTTP cannot reach.
pub(super) fn service_authority(url: &str) -> Option<(String, u16)> {
  let uri: hyper::Uri = url.parse().ok()?;
  if uri.scheme_str() != Some("http") {
    return None;
  }
  let host = strip_brackets(uri.host()?);
  if host.is_empty() {
    return None;
  }
  Some((host.to_string(), uri.port_u16().unwrap_or(80)))
}
