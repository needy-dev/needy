//! Listening and health for every daemon role.

use anyhow::{Context, Result};
use needy_api_proto::v1::health_service_server::HealthService;
use needy_api_proto::v1::{CheckRequest, CheckResponse};
use needy_api_server::{AuthLayer, Authenticator};
use tonic::service::Routes;
use tonic::{Request, Response, Status};

pub struct Health {
    pub instance_id: String,
    pub role: &'static str,
}

#[tonic::async_trait]
impl HealthService for Health {
    async fn check(&self, _: Request<CheckRequest>) -> Result<Response<CheckResponse>, Status> {
        Ok(Response::new(CheckResponse {
            instance_id: self.instance_id.clone(),
            version: env!("CARGO_PKG_VERSION").into(),
            role: self.role.into(),
            platform: format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
        }))
    }
}

/// Accepts the configured token, or anything when no token is configured.
struct Token(Option<String>);

impl Authenticator for Token {
    fn authenticate(&self, token: &str) -> bool {
        let Some(expected) = &self.0 else {
            return true;
        };
        token.len() == expected.len()
            && token
                .bytes()
                .zip(expected.bytes())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0
    }
}

/// Serves `routes` on `listen`: `unix:/path` or `HOST:PORT`.
pub async fn serve(listen: &str, routes: Routes, token: Option<String>) -> Result<()> {
    let layer = AuthLayer::new(Token(token));
    let mut builder = tonic::transport::Server::builder()
        .http2_keepalive_interval(Some(std::time::Duration::from_secs(20)))
        .tcp_nodelay(true)
        .layer(layer);
    let router = builder.add_routes(routes);
    if let Some(path) = listen.strip_prefix("unix:") {
        #[cfg(unix)]
        {
            let _ = std::fs::remove_file(path);
            if let Some(parent) = std::path::Path::new(path).parent() {
                std::fs::create_dir_all(parent).ok();
            }
            let listener = tokio::net::UnixListener::bind(path)
                .with_context(|| format!("cannot listen on {path}"))?;
            router
                .serve_with_incoming(tokio_stream::wrappers::UnixListenerStream::new(listener))
                .await?;
            return Ok(());
        }
        #[cfg(not(unix))]
        anyhow::bail!("unix sockets are not supported here: {path}");
    }
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("cannot listen on {listen}"))?;
    router
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
        .await?;
    Ok(())
}
