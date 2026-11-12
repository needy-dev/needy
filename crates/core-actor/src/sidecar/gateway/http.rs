//! Plain-HTTP requests in absolute form (`GET http://host/path`): a
//! service's host goes to the service's URL, any other allowed host to
//! itself.

use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use needy_core::event::{Event, NetError, NetRequest};

use super::{Body, Gateway, Reset, text};

impl Gateway {
  /// Forwards an absolute-form request to its host, or to the URL of the
  /// service whose host it names.
  pub(super) async fn forward(&self, request: Request<Incoming>) -> Result<Response<Body>, Reset> {
    let Some(host) = request.uri().host().map(str::to_string) else {
      return Ok(text(StatusCode::BAD_REQUEST, "absolute URI required"));
    };
    let decision = self.decide(&host, Some(request.uri().path())).await;
    if let Some(refusal) = Self::refusal(&decision, &host) {
      return refusal;
    }
    let mut target = request.uri().to_string();
    if let Some(base) = self.policy.service(&host) {
      let path = request.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/");
      target = format!("{}{}", base.trim_end_matches('/'), path);
    }
    let (mut parts, body) = request.into_parts();
    let Ok(uri) = target.parse() else {
      return Ok(text(StatusCode::BAD_GATEWAY, "bad upstream"));
    };
    parts.uri = uri;
    parts.headers.remove("proxy-connection");
    parts.headers.remove(hyper::header::HOST);
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
        Ok(response.map(|b| b.boxed()))
      }
      Err(e) => {
        let message = format!("needy: upstream: {e}");
        self.record(Event::NetError(NetError {
          network: self.network,
          host,
          path: Some(path),
          error: e.to_string(),
        }));
        Ok(text(StatusCode::BAD_GATEWAY, &message))
      }
    }
  }
}
