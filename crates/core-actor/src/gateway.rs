//! The actor gateway: an HTTP proxy with an allowlist, upstream mapping and
//! credential injection. The actor reaches it through its in-guest forwarder.
//!
//! - `CONNECT host:port` tunnels when `host` is allowed.
//! - An absolute `http://host/...` request is forwarded; `upstream` may map the
//!   host to another base URL, and `inject` adds a header whose value comes
//!   from the daemon's environment, so the actor never holds the credential.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;

use needy_core::chaos::{Injector, Subject};
use needy_core::event::{Event, NetConnect, NetDeny, NetError, NetRequest};
use needy_core::types::Network;
use needy_trace::Dispatch;

#[derive(Clone, Debug, Default)]
pub struct Policy {
  pub allow: Vec<String>,
  pub upstream: BTreeMap<String, String>,
  /// host -> (header, environment variable holding the value)
  pub inject: BTreeMap<String, (String, String)>,
}

impl Policy {
  pub fn allows(&self, host: &str) -> bool {
    self.allow.iter().any(|pattern| glob_host(pattern, host)) || self.upstream.contains_key(host)
  }
}

fn glob_host(pattern: &str, host: &str) -> bool {
  if pattern == "*" {
    return true;
  }
  if let Some(suffix) = pattern.strip_prefix("*.") {
    return host == suffix || host.ends_with(&format!(".{suffix}"));
  }
  pattern.eq_ignore_ascii_case(host)
}

type Body = BoxBody<Bytes, hyper::Error>;

fn text(status: StatusCode, message: &str) -> Response<Body> {
  Response::builder().status(status).body(Full::new(Bytes::from(format!("{message}\n"))).map_err(|never| match never {}).boxed()).unwrap()
}

pub struct Gateway {
  pub policy: Policy,
  /// The session's trace.
  pub trace: Dispatch,
  pub network: Network,
  injector: Option<Arc<Injector>>,
  client: hyper_util::client::legacy::Client<hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>, Incoming>,
}

impl Gateway {
  pub fn new(policy: Policy, trace: Dispatch, network: Network, injector: Option<Arc<Injector>>) -> Self {
    let https = hyper_rustls::HttpsConnectorBuilder::new().with_webpki_roots().https_or_http().enable_http1().build();
    let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new()).build(https);
    Self {
      policy,
      trace,
      network,
      injector,
      client,
    }
  }

  /// Serves one accepted connection.
  pub async fn serve<S>(self: Arc<Self>, stream: S)
  where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
  {
    let gateway = self.clone();
    let service = service_fn(move |request| {
      let gateway = gateway.clone();
      async move { Ok::<_, Infallible>(gateway.handle(request).await) }
    });
    if let Err(e) = http1::Builder::new().preserve_header_case(true).serve_connection(TokioIo::new(stream), service).with_upgrades().await {
      tracing::debug!("gateway connection for session {}: {e}", self.trace.session());
    }
  }

  /// Applies `net.connect` faults for `host`: delays sleep; an errno fails the request.
  async fn fault(&self, host: &str) -> Option<Response<Body>> {
    let fired = self.injector.as_ref()?.check(Subject::NetConnect(host))?;
    if let Some(delay) = fired.effect.delay {
      tokio::time::sleep(delay).await;
    }
    let errno = fired.effect.errno.clone();
    self.record(Event::FaultInjected(fired));
    errno.map(|errno| text(StatusCode::BAD_GATEWAY, &format!("needy chaos: {errno} for {host}")))
  }

  async fn handle(&self, request: Request<Incoming>) -> Response<Body> {
    if request.method() == Method::CONNECT {
      return self.connect(request).await;
    }
    let Some(host) = request.uri().host().map(str::to_string) else {
      return text(StatusCode::BAD_REQUEST, "absolute URI required");
    };
    if !self.policy.allows(&host) {
      self.record(Event::NetDeny(NetDeny {
        network: self.network,
        host: host.clone(),
        path: Some(request.uri().path().to_string()),
      }));
      return text(StatusCode::FORBIDDEN, &format!("needy: {host} is not allowed"));
    }
    if let Some(response) = self.fault(&host).await {
      return response;
    }
    let mut target = request.uri().to_string();
    if let Some(base) = self.policy.upstream.get(&host) {
      let path = request.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/");
      target = format!("{}{}", base.trim_end_matches('/'), path);
    }
    let (mut parts, body) = request.into_parts();
    let Ok(uri) = target.parse() else {
      return text(StatusCode::BAD_GATEWAY, "bad upstream");
    };
    parts.uri = uri;
    parts.headers.remove("proxy-connection");
    parts.headers.remove(hyper::header::HOST);
    if let Some((header, env)) = self.policy.inject.get(&host)
      && let Ok(value) = std::env::var(env)
      && let (Ok(name), Ok(value)) = (hyper::header::HeaderName::from_bytes(header.as_bytes()), hyper::header::HeaderValue::from_str(&value))
    {
      parts.headers.insert(name, value);
    }
    let path = parts.uri.path().to_string();
    let started = std::time::Instant::now();
    let result = self.client.request(Request::from_parts(parts, body)).await;
    self.trace.time("net.request", started.elapsed());
    match result {
      Ok(response) => {
        self.record(Event::NetRequest(NetRequest {
          network: self.network,
          host,
          path,
          status: response.status().as_u16(),
          ms: started.elapsed().as_millis() as u64,
        }));
        response.map(|b| b.boxed())
      }
      Err(e) => {
        let message = format!("needy: upstream: {e}");
        self.record(Event::NetError(NetError {
          network: self.network,
          host,
          path: Some(path),
          error: e.to_string(),
        }));
        text(StatusCode::BAD_GATEWAY, &message)
      }
    }
  }

  async fn connect(&self, request: Request<Incoming>) -> Response<Body> {
    let Some(authority) = request.uri().authority().map(|a| a.to_string()) else {
      return text(StatusCode::BAD_REQUEST, "CONNECT needs host:port");
    };
    let host = authority.split(':').next().unwrap_or_default().to_string();
    if !self.policy.allows(&host) {
      self.record(Event::NetDeny(NetDeny {
        network: self.network,
        host: host.clone(),
        path: None,
      }));
      return text(StatusCode::FORBIDDEN, &format!("needy: {host} is not allowed"));
    }
    if let Some(response) = self.fault(&host).await {
      return response;
    }
    let started = std::time::Instant::now();
    let connected = tokio::net::TcpStream::connect(&authority).await;
    self.trace.time("net.connect", started.elapsed());
    let upstream = match connected {
      Ok(s) => s,
      Err(e) => {
        let message = format!("needy: {e}");
        self.record(Event::NetError(NetError {
          network: self.network,
          host,
          path: None,
          error: e.to_string(),
        }));
        return text(StatusCode::BAD_GATEWAY, &message);
      }
    };
    self.record(Event::NetConnect(NetConnect {
      network: self.network,
      host,
      authority: authority.clone(),
    }));
    tokio::spawn(async move {
      if let Ok(upgraded) = hyper::upgrade::on(request).await {
        let mut client = TokioIo::new(upgraded);
        let mut upstream = upstream;
        if let Err(e) = tokio::io::copy_bidirectional(&mut client, &mut upstream).await {
          tracing::debug!("tunnel to {authority}: {e}");
        }
      }
    });
    Response::new(Full::new(Bytes::new()).map_err(|never| match never {}).boxed())
  }

  fn record(&self, event: Event) {
    self.trace.emit(event.kind().as_str(), event.raw());
  }
}
