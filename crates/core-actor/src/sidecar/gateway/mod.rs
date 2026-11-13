//! The actor gateway: the HTTP proxy every connection from the actor's
//! network goes through. The actor reaches it through its in-guest
//! forwarder.
//!
//! The gateway is mechanism only: each request ([`http`]) and tunnel
//! ([`connect`]) runs the net domain's chain
//! ([`needy_core::intercept::net::Chain`]: the allowlist, then chaos, which
//! live in `needy_core::network` and `needy_core::chaos`), applies the
//! decision and records typed `net.*` and `fault.injected` events in the
//! run's trace. A plain-HTTP request to a service's host (`NAME.needy`) goes
//! to the service's URL, which this machine resolves: the gateway runs next
//! to the daemon, on the control host.
//!
//! A fault's effect is delivered as a real upstream would deliver it through
//! a proxy: `refuse` answers 502 as when the upstream refuses the
//! connection, `reset` resets the actor's connection without an answer,
//! `hang` holds it until the actor gives up or the run ends, `delay` waits
//! first, and `status` answers a plain-HTTP request with that status.

mod connect;
mod http;

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioIo;
use needy_core::chaos::Injector;
use needy_core::event::{Event, NetDeny};
use needy_core::intercept::net::{Chain, NetContext, NetDecision, NetOutcome};
use needy_core::network::Policy;
use needy_core::types::Fired;
use needy_core::types::Network;
use needy_trace::Dispatch;
use tokio_stream::{Stream, StreamExt};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

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
  /// The allowlist and the services' URLs.
  policy: Policy,
  chain: Chain,
  client: Client<hyper_rustls::HttpsConnector<HttpConnector>, Incoming>,
  cancel: CancellationToken,
  tasks: TaskTracker,
}

impl Gateway {
  /// A gateway for `network` with `policy` and the run's faults in
  /// `injector`, that records in `trace` and stops when its owner cancels
  /// `cancel`.
  pub fn new(policy: Policy, trace: Dispatch, network: Network, injector: Option<Arc<Injector>>, cancel: CancellationToken) -> Self {
    let https = hyper_rustls::HttpsConnectorBuilder::new().with_webpki_roots().https_or_http().enable_http1().build();
    let client = Client::builder(hyper_util::rt::TokioExecutor::new()).build(https);
    Self {
      trace,
      network,
      chain: Chain::new(&policy, injector),
      policy,
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
    let stream = Resettable::new(stream, self.tasks.clone());
    let reset = stream.reset.clone();
    // A service error makes hyper close the connection without writing a
    // response, which is how a `reset` fault is delivered; `Resettable`
    // tells the guest's forwarder first.
    let service = service_fn(move |request| {
      let gateway = gateway.clone();
      let reset = reset.clone();
      async move {
        let handled = gateway.handle(request).await;
        if handled.is_err() {
          reset.store(true, Ordering::SeqCst);
        }
        handled
      }
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

  async fn handle(&self, request: Request<Incoming>) -> Result<Response<Body>, Reset> {
    if request.method() == Method::CONNECT {
      self.connect(request).await
    } else {
      self.forward(request).await
    }
  }

  /// Runs the net chain for `host` and records its outcome.
  /// `path` is the request's path, or `None` for a tunnel. A fault's delay
  /// is waited out here, and a `hang` until the owner cancels.
  async fn decide(&self, host: &str, path: Option<&str>) -> NetDecision {
    let ctx = NetContext {
      network: self.network,
      host: host.to_string(),
      path: path.map(str::to_string),
    };
    let decision = self.chain.decide(&ctx);
    for fired in &decision.fired {
      self.record(Event::FaultInjected(fired.clone()));
      if let Some(delay) = fired.effect.delay {
        tokio::time::sleep(delay).await;
      }
    }
    if decision.fault().is_some_and(|fired| fired.effect.hang) {
      // The connection is dropped when its actor is gone.
      self.cancel.cancelled().await;
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

  /// The response for a request or tunnel the decision refused, or the
  /// [`Reset`] that closes the connection without one.
  fn refusal(decision: &NetDecision, host: &str) -> Option<Result<Response<Body>, Reset>> {
    match &decision.outcome {
      NetOutcome::Allow => None,
      NetOutcome::Deny => Some(Ok(text(StatusCode::FORBIDDEN, &format!("needy: {host} is not allowed")))),
      NetOutcome::Fault(_) => decision.fault().map(|fired| fault_response(fired, host)),
    }
  }

  fn record(&self, event: Event) {
    self.trace.emit(event.kind().as_str(), event.raw());
  }
}

/// A connection to the gateway that, when a `reset` fault closes it before
/// the gateway wrote anything on it, first writes
/// [`GATEWAY_RESET`](crate::contract::GATEWAY_RESET): the guest's forwarder
/// then resets the driving program's connection, since a close alone does
/// not reach every guest.
// NOTICE: vfkit 0.6.4, which carries a `macos-vz` actor's gateway channel,
// does not pass a close to the guest (its `tcpproxy` relay keeps the vsock
// connection open while the guest side is), so a program waited until its
// own timeout there instead of reading a reset (seen 2026-11-13;
// `linux-firecracker` and `macos-apple-container` passed the close). Source:
// https://github.com/crc-org/vfkit/blob/v0.6.4/pkg/vf/vsock.go#L77-L120
// TODO(macos-vz-reset): a reset of a later request on a kept-alive
// plain-HTTP connection, after the gateway answered an earlier one, is still
// only a close, which a `macos-vz` guest does not see: the marker is
// recognized only at a connection's start, where it cannot be part of a
// response. Telling it there needs framing between the forwarder and the
// gateway; add it when a program is seen to depend on it.
struct Resettable<S: tokio::io::AsyncWrite + Unpin + Send + 'static> {
  stream: Option<S>,
  /// Set by the service when a `reset` fault fails a request.
  reset: Arc<AtomicBool>,
  /// Whether the gateway wrote anything on the connection.
  written: bool,
  tasks: TaskTracker,
}

impl<S: tokio::io::AsyncWrite + Unpin + Send + 'static> Resettable<S> {
  fn new(stream: S, tasks: TaskTracker) -> Self {
    Self {
      stream: Some(stream),
      reset: Arc::new(AtomicBool::new(false)),
      written: false,
      tasks,
    }
  }

  fn stream(&mut self) -> std::io::Result<&mut S> {
    self.stream.as_mut().ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotConnected))
  }
}

impl<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static> tokio::io::AsyncRead for Resettable<S> {
  fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut tokio::io::ReadBuf<'_>) -> Poll<std::io::Result<()>> {
    match self.get_mut().stream() {
      Ok(stream) => Pin::new(stream).poll_read(cx, buf),
      Err(e) => Poll::Ready(Err(e)),
    }
  }
}

impl<S: tokio::io::AsyncWrite + Unpin + Send + 'static> tokio::io::AsyncWrite for Resettable<S> {
  fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
    let this = self.get_mut();
    let written = match this.stream() {
      Ok(stream) => Pin::new(stream).poll_write(cx, buf),
      Err(e) => return Poll::Ready(Err(e)),
    };
    if matches!(written, Poll::Ready(Ok(n)) if n > 0) {
      this.written = true;
    }
    written
  }

  fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
    match self.get_mut().stream() {
      Ok(stream) => Pin::new(stream).poll_flush(cx),
      Err(e) => Poll::Ready(Err(e)),
    }
  }

  fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
    match self.get_mut().stream() {
      Ok(stream) => Pin::new(stream).poll_shutdown(cx),
      Err(e) => Poll::Ready(Err(e)),
    }
  }
}

impl<S: tokio::io::AsyncWrite + Unpin + Send + 'static> Drop for Resettable<S> {
  fn drop(&mut self) {
    if self.written || !self.reset.load(Ordering::SeqCst) {
      return;
    }
    let Some(mut stream) = self.stream.take() else {
      return;
    };
    // `Drop` cannot wait for the write: a task the gateway owns does it.
    self.tasks.spawn(async move {
      use tokio::io::AsyncWriteExt;
      if let Err(e) = stream.write_all(crate::contract::GATEWAY_RESET).await {
        tracing::debug!("cannot tell the forwarder of a reset: {e}");
      }
      // The close follows when the stream is dropped; a shutdown error
      // means the forwarder went away, which needs no reset any more.
      let _ = stream.shutdown().await;
    });
  }
}

/// What a fault that fails a request or tunnel answers.
fn fault_response(fired: &Fired, host: &str) -> Result<Response<Body>, Reset> {
  let effect = &fired.effect;
  if effect.reset {
    return Err(Reset(fired.name.clone()));
  }
  if let Some(status) = effect.status
    && let Ok(status) = StatusCode::from_u16(status)
  {
    return Ok(text(status, &format!("needy chaos: {}", fired.name)));
  }
  // `refuse`, and a `hang` the owner cancelled: worded as the gateway words
  // a real upstream's refusal (`needy: Connection refused ...`).
  Ok(text(StatusCode::BAD_GATEWAY, &format!("needy: {host}: connection refused (needy chaos: {})", fired.name)))
}

/// A `reset` fault: the service fails, and hyper closes the connection
/// without a response (after [`Resettable`]'s marker, at a connection's
/// start).
#[derive(Debug)]
pub(crate) struct Reset(String);

impl std::fmt::Display for Reset {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "needy chaos: {} resets the connection", self.0)
  }
}

impl std::error::Error for Reset {}

#[cfg(all(test, unix))]
#[path = "mod_test.rs"]
mod tests;
