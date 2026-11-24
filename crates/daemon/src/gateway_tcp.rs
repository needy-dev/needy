//! Shared authenticated TCP gateway listener for remote actor runners.
//!
//! Each remote actor run registers its [`Gateway`] with the daemon. The
//! listener authenticates connections using the Node Authorizer (the actor
//! runner's credential plus the bound run ID) in two styles:
//!
//! 1. **Bridge preamble** (used by `needy runner serve --roles actor`
//!    forwarding guest vsock port 7001): the first line is
//!    `NEEDY-GATEWAY <runner_credential> <run_id>\n`, followed by raw guest
//!    HTTP/CONNECT traffic. The daemon authenticates the runner credential,
//!    authorizes `(runner, run_id)` via the Node Authorizer, resolves the
//!    run's gateway, and hands the remaining stream to
//!    [`Gateway::serve_authenticated`].
//! 2. **Direct HTTP proxy** (used by clients/tests sending
//!    `Proxy-Authorization: Bearer <runner_credential>` and
//!    `X-Needy-Run-Id: <run_id>`): the daemon peeks the HTTP request headers,
//!    authenticates and authorizes `(runner, run_id)`, and replays the
//!    buffered bytes into [`Gateway::serve_authenticated`], which strips
//!    `Proxy-Authorization` and `X-Needy-Run-Id` before forwarding.

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use anyhow::{Context as _, Result};
use needy_api_server::control::{Credential, Runners as _};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use crate::Daemon;

/// Connection preamble prefix written by an actor runner before raw guest
/// HTTP/CONNECT bytes on a shared TCP gateway connection:
/// `NEEDY-GATEWAY <runner_credential> <run_id>\n`.
pub const PREAMBLE_PREFIX: &str = "NEEDY-GATEWAY ";

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_HEADER_BYTES: usize = 16 * 1024;

const PROXY_AUTH_REQUIRED_RESPONSE: &[u8] =
  b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Bearer\r\nContent-Type: text/plain\r\nContent-Length: 36\r\nConnection: close\r\n\r\nneedy: proxy authentication required\n";

/// A bound shared TCP gateway listener.
pub struct BoundGateway {
  listener: TcpListener,
  daemon: Daemon,
}

impl BoundGateway {
  pub fn local_addr(&self) -> std::io::Result<std::net::SocketAddr> {
    self.listener.local_addr()
  }

  /// Serves accepted connections until `stop` is cancelled.
  pub async fn serve(self, stop: CancellationToken) {
    loop {
      tokio::select! {
        () = stop.cancelled() => break,
        accepted = self.listener.accept() => match accepted {
          Ok((stream, _peer)) => {
            let daemon = self.daemon.clone();
            self.daemon.tasks.spawn(async move {
              handle_connection(daemon, stream).await;
            });
          }
          Err(e) => {
            tracing::warn!("tcp gateway accept error: {e}");
            break;
          }
        }
      }
    }
  }
}

impl Daemon {
  /// Binds the shared authenticated TCP gateway on `address` (`HOST:PORT`).
  pub async fn bind_gateway_tcp(&self, address: &str) -> Result<BoundGateway> {
    let listener = TcpListener::bind(address).await.with_context(|| format!("cannot bind tcp gateway on {address}"))?;
    Ok(BoundGateway {
      listener,
      daemon: self.clone(),
    })
  }
}

async fn handle_connection(daemon: Daemon, mut stream: TcpStream) {
  let _ = stream.set_nodelay(true);
  let mut buf = Vec::with_capacity(512);
  let mut chunk = [0u8; 1024];

  let read_initial = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
    loop {
      let n = stream.read(&mut chunk).await?;
      if n == 0 {
        return Ok::<_, std::io::Error>(None);
      }
      buf.extend_from_slice(&chunk[..n]);
      // Case 1: Bridge preamble (`NEEDY-GATEWAY <token>\n`).
      if buf.starts_with(PREAMBLE_PREFIX.as_bytes()) {
        if let Some(nl) = buf.iter().position(|&b| b == b'\n') {
          return Ok(Some((true, nl + 1)));
        }
      } else if !PREAMBLE_PREFIX.as_bytes().starts_with(&buf) {
        // Case 2: Direct HTTP/1.1 request; wait for end of headers (`\r\n\r\n`).
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
          return Ok(Some((false, 0)));
        }
      }
      if buf.len() >= MAX_HEADER_BYTES {
        return Ok(None);
      }
    }
  })
  .await;

  let (is_preamble, consumed) = match read_initial {
    Ok(Ok(Some(res))) => res,
    _ => return,
  };

  if is_preamble {
    let line = String::from_utf8_lossy(&buf[PREAMBLE_PREFIX.len()..consumed]);
    let Some((credential, run_id)) = parse_preamble(line.trim()) else {
      tracing::debug!("tcp gateway: malformed bridge preamble");
      return;
    };
    let Some(run) = authorize_runner(&daemon, credential, run_id).await else {
      tracing::debug!("tcp gateway: rejected bridge preamble for run {run_id}");
      return;
    };
    let Some(gateway) = run.gateway() else {
      tracing::debug!("tcp gateway: run {} has no active gateway", run.id);
      return;
    };
    let remainder = buf.split_off(consumed);
    gateway.serve_authenticated(PrefixedStream::new(remainder, stream)).await;
  } else {
    let Some((credential, run_id)) = extract_proxy_auth(&buf) else {
      let _ = stream.write_all(PROXY_AUTH_REQUIRED_RESPONSE).await;
      let _ = stream.shutdown().await;
      return;
    };
    let Some(run) = authorize_runner(&daemon, &credential, &run_id).await else {
      let _ = stream.write_all(PROXY_AUTH_REQUIRED_RESPONSE).await;
      let _ = stream.shutdown().await;
      return;
    };
    let Some(gateway) = run.gateway() else {
      let _ = stream.write_all(PROXY_AUTH_REQUIRED_RESPONSE).await;
      let _ = stream.shutdown().await;
      return;
    };
    gateway.serve_authenticated(PrefixedStream::new(buf, stream)).await;
  }
}

/// Parses `<runner_credential> <run_id>` from the bridge preamble line.
fn parse_preamble(line: &str) -> Option<(&str, &str)> {
  let mut parts = line.split_whitespace();
  let credential = parts.next()?;
  let run_id = parts.next()?;
  if parts.next().is_some() || credential.is_empty() || run_id.is_empty() {
    return None;
  }
  Some((credential, run_id))
}

/// Authenticates `credential` as a runner and checks the Node Authorizer for
/// `run_id`.
async fn authorize_runner(daemon: &Daemon, credential: &str, run_id: &str) -> Option<std::sync::Arc<crate::run::Run>> {
  let Ok(Credential::Runner(runner)) = daemon.credential(credential).await else {
    return None;
  };
  daemon.authorized_actor_run(&runner, run_id)
}

/// Extracts `(<token>, <run_id>)` from `Proxy-Authorization: Bearer <token>`
/// and `X-Needy-Run-Id: <run_id>` in a raw HTTP/1.1 header block.
fn extract_proxy_auth(headers: &[u8]) -> Option<(String, String)> {
  let text = std::str::from_utf8(headers).ok()?;
  let mut token = None;
  let mut run_id = None;
  for line in text.split("\r\n").skip(1) {
    if line.is_empty() {
      break;
    }
    let Some((name, value)) = line.split_once(':') else {
      continue;
    };
    let name = name.trim();
    if name.eq_ignore_ascii_case("proxy-authorization") {
      let value = value.trim();
      let (scheme, raw) = value.split_once(' ')?;
      if scheme.eq_ignore_ascii_case("bearer") && !raw.trim().is_empty() {
        token = Some(raw.trim().to_string());
      }
    } else if name.eq_ignore_ascii_case(needy_api_server::middleware::authentication::RUN_ID_HEADER) {
      let value = value.trim();
      if !value.is_empty() {
        run_id = Some(value.to_string());
      }
    }
  }
  Some((token?, run_id?))
}

/// Prepends already-read bytes back onto an underlying async stream.
struct PrefixedStream<S> {
  prefix: Vec<u8>,
  offset: usize,
  inner: S,
}

impl<S> PrefixedStream<S> {
  fn new(prefix: Vec<u8>, inner: S) -> Self {
    Self {
      prefix,
      offset: 0,
      inner,
    }
  }
}

impl<S: AsyncRead + Unpin> AsyncRead for PrefixedStream<S> {
  fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
    if self.offset < self.prefix.len() {
      let remaining = &self.prefix[self.offset..];
      let n = remaining.len().min(buf.remaining());
      buf.put_slice(&remaining[..n]);
      self.offset += n;
      return Poll::Ready(Ok(()));
    }
    Pin::new(&mut self.inner).poll_read(cx, buf)
  }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for PrefixedStream<S> {
  fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
    Pin::new(&mut self.inner).poll_write(cx, buf)
  }

  fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
    Pin::new(&mut self.inner).poll_flush(cx)
  }

  fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
    Pin::new(&mut self.inner).poll_shutdown(cx)
  }
}
