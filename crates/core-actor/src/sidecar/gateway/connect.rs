//! `CONNECT host:port` tunnels. The tunnel is opaque: a service is not
//! reached this way, and a fault cannot answer with a status.

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
    let started = std::time::Instant::now();
    let connected = tokio::net::TcpStream::connect(&authority).await;
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
