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

use needy_core::trace::Trace;

#[derive(Clone, Debug, Default)]
pub struct Policy {
    pub allow: Vec<String>,
    pub upstream: BTreeMap<String, String>,
    /// host -> (header, environment variable holding the value)
    pub inject: BTreeMap<String, (String, String)>,
}

impl Policy {
    pub fn allows(&self, host: &str) -> bool {
        self.allow.iter().any(|pattern| glob_host(pattern, host))
            || self.upstream.contains_key(host)
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
    Response::builder()
        .status(status)
        .body(
            Full::new(Bytes::from(format!("{message}\n")))
                .map_err(|never| match never {})
                .boxed(),
        )
        .unwrap()
}

pub struct Gateway {
    pub policy: Policy,
    pub trace: Trace,
    pub session: String,
    pub network: &'static str,
    injector: Option<Arc<needy_core::chaos::Injector>>,
    client: hyper_util::client::legacy::Client<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        Incoming,
    >,
}

impl Gateway {
    pub fn new(
        policy: Policy,
        trace: Trace,
        session: String,
        network: &'static str,
        injector: Option<Arc<needy_core::chaos::Injector>>,
    ) -> Self {
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .build();
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build(https);
        Self {
            policy,
            trace,
            session,
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
        let _ = http1::Builder::new()
            .preserve_header_case(true)
            .serve_connection(TokioIo::new(stream), service)
            .with_upgrades()
            .await;
    }

    /// Applies `net.connect` faults for `host`: delays sleep; an errno fails the request.
    async fn fault(&self, host: &str) -> Option<Response<Body>> {
        let fired = self.injector.as_ref()?.check("net.connect", host)?;
        if let Some(delay) = fired.delay {
            tokio::time::sleep(delay).await;
        }
        self.trace.record(
            &self.session,
            "fault.injected",
            [
                ("on", "net.connect".to_string()),
                ("host", host.to_string()),
                ("errno", fired.errno.clone().unwrap_or_default()),
                (
                    "delay_ms",
                    fired
                        .delay
                        .map(|d| d.as_millis().to_string())
                        .unwrap_or_default(),
                ),
            ],
        );
        fired.errno.map(|errno| {
            text(
                StatusCode::BAD_GATEWAY,
                &format!("needy chaos: {errno} for {host}"),
            )
        })
    }

    async fn handle(&self, request: Request<Incoming>) -> Response<Body> {
        if request.method() == Method::CONNECT {
            return self.connect(request).await;
        }
        let Some(host) = request.uri().host().map(str::to_string) else {
            return text(StatusCode::BAD_REQUEST, "absolute URI required");
        };
        if !self.policy.allows(&host) {
            self.record("net.deny", &host, request.uri().path(), "");
            return text(
                StatusCode::FORBIDDEN,
                &format!("needy: {host} is not allowed"),
            );
        }
        if let Some(response) = self.fault(&host).await {
            return response;
        }
        let mut target = request.uri().to_string();
        if let Some(base) = self.policy.upstream.get(&host) {
            let path = request
                .uri()
                .path_and_query()
                .map(|p| p.as_str())
                .unwrap_or("/");
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
            && let (Ok(name), Ok(value)) = (
                hyper::header::HeaderName::from_bytes(header.as_bytes()),
                hyper::header::HeaderValue::from_str(&value),
            )
        {
            parts.headers.insert(name, value);
        }
        let path = parts.uri.path().to_string();
        let started = std::time::Instant::now();
        match self.client.request(Request::from_parts(parts, body)).await {
            Ok(response) => {
                self.record(
                    "net.request",
                    &host,
                    &path,
                    &format!(
                        "{} {}ms",
                        response.status().as_u16(),
                        started.elapsed().as_millis()
                    ),
                );
                response.map(|b| b.boxed())
            }
            Err(e) => {
                self.record("net.error", &host, &path, &e.to_string());
                text(StatusCode::BAD_GATEWAY, &format!("needy: upstream: {e}"))
            }
        }
    }

    async fn connect(&self, request: Request<Incoming>) -> Response<Body> {
        let Some(authority) = request.uri().authority().map(|a| a.to_string()) else {
            return text(StatusCode::BAD_REQUEST, "CONNECT needs host:port");
        };
        let host = authority.split(':').next().unwrap_or_default().to_string();
        if !self.policy.allows(&host) {
            self.record("net.deny", &host, "", "CONNECT");
            return text(
                StatusCode::FORBIDDEN,
                &format!("needy: {host} is not allowed"),
            );
        }
        if let Some(response) = self.fault(&host).await {
            return response;
        }
        let upstream = match tokio::net::TcpStream::connect(&authority).await {
            Ok(s) => s,
            Err(e) => {
                self.record("net.error", &host, "", &e.to_string());
                return text(StatusCode::BAD_GATEWAY, &format!("needy: {e}"));
            }
        };
        self.record("net.connect", &host, "", &authority);
        tokio::spawn(async move {
            if let Ok(upgraded) = hyper::upgrade::on(request).await {
                let mut client = TokioIo::new(upgraded);
                let mut upstream = upstream;
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            }
        });
        Response::new(
            Full::new(Bytes::new())
                .map_err(|never| match never {})
                .boxed(),
        )
    }

    fn record(&self, kind: &str, host: &str, path: &str, detail: &str) {
        self.trace.record(
            &self.session,
            kind,
            [
                ("network", self.network.to_string()),
                ("host", host.to_string()),
                ("path", path.to_string()),
                ("detail", detail.to_string()),
            ],
        );
    }
}
