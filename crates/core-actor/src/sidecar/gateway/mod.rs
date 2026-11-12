//! The actor gateway: the HTTP proxy every connection from the actor's
//! network goes through. The actor reaches it through its in-guest
//! forwarder.
//!
//! Each request ([`http`]) and tunnel ([`connect`]) runs the `net.request`
//! or `net.connect` interceptors in order: the [`Allowlist`], chaos
//! ([`needy_core::chaos::Injector`]) and header injection ([`http::Inject`]).
//! The gateway applies the decision and records typed `net.*` and
//! `fault.injected` events in the run's trace.

mod connect;
mod http;

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioIo;
use needy_config::value::Glob;
use needy_core::chaos::Injector;
use needy_core::event::{Event, NetDeny};
use needy_core::intercept::{self, NetContext, NetDecide};
use needy_core::types::{NetDecision, NetOutcome, Network};
use needy_trace::Dispatch;
use tokio_stream::{Stream, StreamExt};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// A network's policy at its gateway.
// NOTICE: no `Debug`: `inject` holds credentials.
#[derive(Clone, Default)]
pub struct Policy {
  /// Host patterns that may be reached, matched by [`Glob::matches_host`]:
  /// `*`, `*.example.com` (which also matches `example.com`), or a host, in
  /// any case.
  pub allow: Vec<Glob>,
  /// Host to the base URL its plain-HTTP requests are forwarded to. A mapped
  /// host is allowed.
  pub upstream: BTreeMap<String, String>,
  /// Host to the header and value added to its requests. The value is the
  /// credential itself, read by the daemon, so the actor never holds it.
  pub inject: BTreeMap<String, (String, String)>,
}

/// The first `net.*` interceptor: a host that matches no pattern and has no
/// upstream is denied.
struct Allowlist {
  patterns: Vec<Glob>,
  upstreams: Vec<String>,
}

impl Allowlist {
  fn allows(&self, host: &str) -> bool {
    self.patterns.iter().any(|pattern| pattern.matches_host(host)) || self.upstreams.iter().any(|upstream| upstream == host)
  }
}

impl NetDecide for Allowlist {
  fn decide(&self, ctx: &NetContext, mut decision: NetDecision) -> NetDecision {
    if decision.outcome == NetOutcome::Allow && !self.allows(&ctx.host) {
      decision.outcome = NetOutcome::Deny;
    }
    decision
  }
}

type Body = BoxBody<Bytes, hyper::Error>;

fn text(status: StatusCode, message: &str) -> Response<Body> {
  let mut response = Response::new(Full::new(Bytes::from(format!("{message}\n"))).map_err(|never| match never {}).boxed());
  *response.status_mut() = status;
  response
}

/// One network's gateway. Its connections and tunnels are tasks it owns,
/// under its owner's cancellation token.
pub struct Gateway {
  /// The run's trace.
  trace: Dispatch,
  network: Network,
  upstream: BTreeMap<String, String>,
  allowlist: Allowlist,
  injector: Option<Arc<Injector>>,
  inject: http::Inject,
  client: Client<hyper_rustls::HttpsConnector<HttpConnector>, Incoming>,
  cancel: CancellationToken,
  tasks: TaskTracker,
}

impl Gateway {
  /// A gateway for `network` that records in `trace` and stops when its
  /// owner cancels `cancel`.
  pub fn new(policy: Policy, trace: Dispatch, network: Network, injector: Option<Arc<Injector>>, cancel: CancellationToken) -> Self {
    let https = hyper_rustls::HttpsConnectorBuilder::new().with_webpki_roots().https_or_http().enable_http1().build();
    let client = Client::builder(hyper_util::rt::TokioExecutor::new()).build(https);
    Self {
      trace,
      network,
      allowlist: Allowlist {
        patterns: policy.allow,
        upstreams: policy.upstream.keys().cloned().collect(),
      },
      upstream: policy.upstream,
      injector,
      inject: http::Inject::new(policy.inject),
      client,
      cancel,
      tasks: TaskTracker::new(),
    }
  }

  /// Serves the connections `incoming` yields until the owner's token is
  /// cancelled, then ends every open connection and tunnel and waits for
  /// them.
  pub async fn run<S>(self: Arc<Self>, mut incoming: impl Stream<Item = std::io::Result<S>> + Unpin)
  where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
  {
    loop {
      tokio::select! {
        () = self.cancel.cancelled() => break,
        accepted = incoming.next() => match accepted {
          Some(Ok(stream)) => {
            self.tasks.spawn(self.clone().serve(stream));
          }
          Some(Err(e)) => {
            tracing::warn!("gateway for run {}: cannot accept: {e}", self.trace.run());
            break;
          }
          None => break,
        },
      }
    }
    // When accepting stopped first, the open connections and tunnels still
    // run until they close or the owner cancels.
    self.tasks.close();
    self.tasks.wait().await;
  }

  /// Serves one accepted connection until it closes or the owner cancels.
  async fn serve<S>(self: Arc<Self>, stream: S)
  where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
  {
    let gateway = self.clone();
    let service = service_fn(move |request| {
      let gateway = gateway.clone();
      async move { Ok::<_, Infallible>(gateway.handle(request).await) }
    });
    let connection = http1::Builder::new().preserve_header_case(true).serve_connection(TokioIo::new(stream), service).with_upgrades();
    tokio::select! {
      result = connection => {
        if let Err(e) = result {
          tracing::debug!("gateway connection for run {}: {e}", self.trace.run());
        }
      }
      // The connection is dropped: its actor is gone.
      () = self.cancel.cancelled() => {}
    }
  }

  async fn handle(&self, request: Request<Incoming>) -> Response<Body> {
    if request.method() == Method::CONNECT {
      self.connect(request).await
    } else {
      self.forward(request).await
    }
  }

  /// Runs the `net.*` interceptors for `host` and records their outcome.
  /// `path` is the request's path, or `None` for a tunnel. A fault's delay
  /// is waited out here.
  async fn decide(&self, host: &str, path: Option<&str>) -> NetDecision {
    let ctx = NetContext {
      network: self.network,
      host: host.to_string(),
      path: path.map(str::to_string),
    };
    let decision = {
      let mut chain: Vec<&dyn NetDecide> = vec![&self.allowlist];
      if let Some(injector) = &self.injector {
        chain.push(injector.as_ref());
      }
      chain.push(&self.inject);
      intercept::decide_net(&chain, &ctx, NetDecision::allowed())
    };
    for fired in &decision.fired {
      if let Some(delay) = fired.effect.delay {
        tokio::time::sleep(delay).await;
      }
      self.record(Event::FaultInjected(fired.clone()));
    }
    if decision.outcome == NetOutcome::Deny {
      self.record(Event::NetDeny(NetDeny {
        network: self.network,
        host: ctx.host,
        path: ctx.path,
      }));
    }
    decision
  }

  /// The response for a request or tunnel the decision refused.
  fn refusal(decision: &NetDecision, host: &str) -> Option<Response<Body>> {
    match &decision.outcome {
      NetOutcome::Allow => None,
      NetOutcome::Deny => Some(text(StatusCode::FORBIDDEN, &format!("needy: {host} is not allowed"))),
      NetOutcome::Fault(errno) => Some(text(StatusCode::BAD_GATEWAY, &format!("needy chaos: {errno} for {host}"))),
    }
  }

  fn record(&self, event: Event) {
    self.trace.emit(event.kind().as_str(), event.raw());
  }
}

#[cfg(all(test, unix))]
#[path = "mod_test.rs"]
mod tests;
