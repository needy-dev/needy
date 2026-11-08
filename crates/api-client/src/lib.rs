//! Client for the Needy daemon API.
//!
//! [`Client`] owns one gRPC channel and hands out a typed client per service.
//! Every call carries the paired bearer token when one is set.

use needy_api_proto::daemon::v1::health_service_client::HealthServiceClient;
use needy_api_proto::daemon::v1::host_service_client::HostServiceClient;
use needy_api_proto::daemon::v1::pairing_service_client::PairingServiceClient;
use needy_api_proto::runtime::v1::process_service_client::ProcessServiceClient;
use needy_api_proto::runtime::v1::run_service_client::RunServiceClient;
use needy_api_proto::runtime::v1::session_service_client::SessionServiceClient;
use tonic::metadata::{AsciiMetadataValue, MetadataValue};
use tonic::service::Interceptor;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};
use tonic::{Request, Status};

pub use needy_api_proto as proto;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid endpoint: {0}")]
    InvalidEndpoint(String),
    #[error("token is not a valid header value")]
    InvalidToken,
    #[error(transparent)]
    Transport(#[from] tonic::transport::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Adds `authorization: Bearer <token>` to every request when a token is set.
#[derive(Debug, Clone, Default)]
pub struct BearerAuth(Option<AsciiMetadataValue>);

impl Interceptor for BearerAuth {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        if let Some(value) = &self.0 {
            request
                .metadata_mut()
                .insert("authorization", value.clone());
        }
        Ok(request)
    }
}

pub type Service = InterceptedService<Channel, BearerAuth>;

#[derive(Debug, Clone)]
pub struct Client {
    channel: Channel,
    auth: BearerAuth,
}

impl Client {
    /// Connects to `http://` or `https://` daemon endpoints. HTTPS uses the
    /// platform's root certificates.
    pub async fn connect(url: impl Into<String>) -> Result<Self> {
        let url = url.into();
        let mut endpoint =
            Endpoint::from_shared(url.clone()).map_err(|_| Error::InvalidEndpoint(url.clone()))?;
        if url.starts_with("https://") {
            endpoint = endpoint.tls_config(ClientTlsConfig::new().with_native_roots())?;
        }
        Ok(Self::from_channel(endpoint.connect().await?))
    }

    /// Wraps an existing channel, for example one with a custom TLS setup.
    pub fn from_channel(channel: Channel) -> Self {
        Self {
            channel,
            auth: BearerAuth::default(),
        }
    }

    pub fn with_token(mut self, token: &str) -> Result<Self> {
        let value =
            MetadataValue::try_from(format!("Bearer {token}")).map_err(|_| Error::InvalidToken)?;
        self.auth = BearerAuth(Some(value));
        Ok(self)
    }

    fn service(&self) -> Service {
        InterceptedService::new(self.channel.clone(), self.auth.clone())
    }

    pub fn health(&self) -> HealthServiceClient<Service> {
        HealthServiceClient::new(self.service())
    }

    pub fn host(&self) -> HostServiceClient<Service> {
        HostServiceClient::new(self.service())
    }

    pub fn pairing(&self) -> PairingServiceClient<Service> {
        PairingServiceClient::new(self.service())
    }

    pub fn sessions(&self) -> SessionServiceClient<Service> {
        SessionServiceClient::new(self.service())
    }

    pub fn runs(&self) -> RunServiceClient<Service> {
        RunServiceClient::new(self.service())
    }

    pub fn processes(&self) -> ProcessServiceClient<Service> {
        ProcessServiceClient::new(self.service())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use needy_api_proto::daemon::v1::health_service_server::{HealthService, HealthServiceServer};
    use needy_api_proto::daemon::v1::{CheckRequest, CheckResponse, HealthStatus};
    use tokio::net::TcpListener;
    use tokio_stream::wrappers::TcpListenerStream;
    use tonic::Response;

    struct Health;

    #[tonic::async_trait]
    impl HealthService for Health {
        async fn check(
            &self,
            request: Request<CheckRequest>,
        ) -> Result<Response<CheckResponse>, Status> {
            let auth = request.metadata().get("authorization");
            if auth.and_then(|v| v.to_str().ok()) != Some("Bearer secret") {
                return Err(Status::unauthenticated("missing token"));
            }
            Ok(Response::new(CheckResponse {
                status: HealthStatus::Serving.into(),
                instance_id: "test".into(),
                version: String::new(),
            }))
        }
    }

    #[tokio::test]
    async fn sends_bearer_token() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(HealthServiceServer::new(Health))
                .serve_with_incoming(TcpListenerStream::new(listener)),
        );

        let client = Client::connect(url).await.unwrap();
        let status = client.health().check(CheckRequest {}).await.unwrap_err();
        assert_eq!(status.code(), tonic::Code::Unauthenticated);

        let response = client
            .with_token("secret")
            .unwrap()
            .health()
            .check(CheckRequest {})
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.status(), HealthStatus::Serving);
    }
}
