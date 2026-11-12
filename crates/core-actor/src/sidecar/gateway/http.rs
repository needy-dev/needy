//! Plain-HTTP requests in absolute form (`GET http://host/path`): upstream
//! mapping, header injection and forwarding.

use std::collections::BTreeMap;

use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::header::{HeaderName, HeaderValue};
use hyper::{Request, Response, StatusCode};
use needy_core::event::{Event, NetError, NetRequest};
use needy_core::intercept::{NetContext, NetDecide};
use needy_core::types::{NetDecision, NetOutcome};

use super::{Body, Gateway, text};

/// The last `net.request` interceptor: adds the configured header to an
/// allowed request for its host. Tunnels are opaque and get nothing.
pub(super) struct Inject {
  /// Host to header name and value.
  headers: BTreeMap<String, (String, String)>,
}

impl Inject {
  pub(super) fn new(headers: BTreeMap<String, (String, String)>) -> Self {
    Self { headers }
  }
}

impl NetDecide for Inject {
  fn decide(&self, ctx: &NetContext, mut decision: NetDecision) -> NetDecision {
    if decision.outcome == NetOutcome::Allow
      && ctx.path.is_some()
      && let Some((name, value)) = self.headers.get(&ctx.host)
    {
      decision.headers.push((name.clone(), value.clone()));
    }
    decision
  }
}

impl Gateway {
  /// Forwards an absolute-form request to its host, or to the host's
  /// upstream.
  pub(super) async fn forward(&self, request: Request<Incoming>) -> Response<Body> {
    let Some(host) = request.uri().host().map(str::to_string) else {
      return text(StatusCode::BAD_REQUEST, "absolute URI required");
    };
    let decision = self.decide(&host, Some(request.uri().path())).await;
    if let Some(refusal) = Self::refusal(&decision, &host) {
      return refusal;
    }
    let mut target = request.uri().to_string();
    if let Some(base) = self.upstream.get(&host) {
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
    for (name, value) in decision.headers {
      match (HeaderName::from_bytes(name.as_bytes()), HeaderValue::from_str(&value)) {
        (Ok(name), Ok(value)) => {
          parts.headers.insert(name, value);
        }
        // The value is a credential; only the header's name is logged.
        _ => tracing::warn!("gateway for run {}: cannot inject header {name:?} for {host}", self.trace.run()),
      }
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
}
