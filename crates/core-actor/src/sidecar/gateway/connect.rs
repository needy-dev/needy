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
use needy_core::event::{Event, NetConnect, NetError};

use super::{Body, Gateway, Reset, text};

impl Gateway {
  /// Opens a tunnel to the request's authority. The tunnel is a task the
  /// gateway owns; it ends with either side or the owner.
  pub(super) async fn connect(&self, request: Request<Incoming>) -> Result<Response<Body>, Reset> {
    let Some(authority) = request.uri().authority().map(|a| a.to_string()) else {
      return Ok(text(StatusCode::BAD_REQUEST, "CONNECT needs host:port"));
    };
    let host = authority.split(':').next().unwrap_or_default().to_string();
    let decision = self.decide(&host, None).await;
    if let Some(refusal) = Self::refusal(&decision, &host) {
      return refusal;
    }
    let upstream_authority = match self.policy.service(&host) {
      None => authority.clone(),
      Some(url) => match service_authority(url) {
        Some(upstream) => upstream,
        None => return Ok(text(StatusCode::BAD_GATEWAY, "needy: the service's URL is not plain http://host[:port]")),
      },
    };
    let started = std::time::Instant::now();
    let connected = tokio::net::TcpStream::connect(&upstream_authority).await;
    self.trace.time("net.connect", started.elapsed());
    let mut upstream = match connected {
      Ok(stream) => stream,
      Err(e) => {
        let message = format!("needy: {e}");
        self.record(Event::NetError(NetError {
          network: self.network,
          host,
          path: None,
          error: e.to_string(),
        }));
        return Ok(text(StatusCode::BAD_GATEWAY, &message));
      }
    };
    self.record(Event::NetConnect(NetConnect {
      network: self.network,
      host,
      authority: authority.clone(),
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

/// The `host:port` a plain-HTTP service URL names (port 80 by default); none
/// for another scheme, which a tunnel of plain HTTP cannot reach.
fn service_authority(url: &str) -> Option<String> {
  let uri: hyper::Uri = url.parse().ok()?;
  if uri.scheme_str() != Some("http") {
    return None;
  }
  Some(format!("{}:{}", uri.host()?, uri.port_u16().unwrap_or(80)))
}
