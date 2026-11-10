//! Client for the Needy daemon API.
//!
//! [`Client`] owns one gRPC channel and hands out a typed client per service.
//! Endpoints are `http://`, `https://`, `unix:/path` and, inside a Linux guest,
//! `vsock://CID:PORT`. Every call carries the bearer token when the caller
//! sets one; the library never reads a token from the environment.
//!
//! - [`ready`]: connect and wait until the server answers its health check.
//! - [`exec`]: exec streams: start or attach, input, events, run to
//!   completion.
//! - [`files`]: tar archives over exec streams, to and from an executor.
//! - [`error`]: errors, with the server's status mapped to a [`Kind`].

pub mod error;
pub mod exec;
pub mod files;
pub mod ready;

use needy_api_proto::v1::guest_service_client::GuestServiceClient;
use needy_api_proto::v1::health_service_client::HealthServiceClient;
use needy_api_proto::v1::process_service_client::ProcessServiceClient;
use needy_api_proto::v1::run_service_client::RunServiceClient;
use needy_api_proto::v1::runner_service_client::RunnerServiceClient;
use tonic::metadata::{AsciiMetadataValue, MetadataValue};
use tonic::service::Interceptor;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint, Uri};
use tonic::{Request, Status};

pub use error::{Error, Kind, Result};
pub use needy_api_proto as proto;

/// Adds `authorization: Bearer <token>` to every request when a token is set.
#[derive(Debug, Clone, Default)]
pub struct BearerAuth(Option<AsciiMetadataValue>);

impl Interceptor for BearerAuth {
  fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
    if let Some(value) = &self.0 {
      request.metadata_mut().insert("authorization", value.clone());
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
  /// Connects to a daemon endpoint. HTTPS uses the platform's root
  /// certificates. A malformed endpoint, or one this platform cannot reach
  /// (vsock outside Linux), is [`Error::InvalidEndpoint`].
  pub async fn connect(url: &str) -> Result<Self> {
    if let Some(path) = url.strip_prefix("unix:") {
      return Ok(Self::from_channel(connect_unix(path.to_string()).await?));
    }
    if let Some(rest) = url.strip_prefix("vsock://") {
      let (cid, port) = rest
        .split_once(':')
        .and_then(|(c, p)| Some((c.parse::<u32>().ok()?, p.parse::<u32>().ok()?)))
        .ok_or_else(|| Error::InvalidEndpoint(url.to_string()))?;
      return Ok(Self::from_channel(connect_vsock(cid, port).await?));
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
      return Err(Error::InvalidEndpoint(url.to_string()));
    }
    let mut endpoint = Endpoint::from_shared(url.to_string()).map_err(|_| Error::InvalidEndpoint(url.to_string()))?;
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
    let value = MetadataValue::try_from(format!("Bearer {token}")).map_err(|_| Error::InvalidToken)?;
    self.auth = BearerAuth(Some(value));
    Ok(self)
  }

  fn service(&self) -> Service {
    InterceptedService::new(self.channel.clone(), self.auth.clone())
  }

  pub fn health(&self) -> HealthServiceClient<Service> {
    HealthServiceClient::new(self.service())
  }

  pub fn processes(&self) -> ProcessServiceClient<Service> {
    ProcessServiceClient::new(self.service()).max_decoding_message_size(usize::MAX).max_encoding_message_size(usize::MAX)
  }

  pub fn runs(&self) -> RunServiceClient<Service> {
    RunServiceClient::new(self.service())
  }

  pub fn runners(&self) -> RunnerServiceClient<Service> {
    RunnerServiceClient::new(self.service())
  }

  pub fn guest(&self) -> GuestServiceClient<Service> {
    GuestServiceClient::new(self.service())
  }
}

fn placeholder() -> Endpoint {
  // The URI is ignored by custom connectors but must parse.
  Endpoint::from_static("http://needy.local")
}

#[cfg(unix)]
async fn connect_unix(path: String) -> Result<Channel> {
  Ok(
    placeholder()
      .connect_with_connector(tower::service_fn(move |_: Uri| {
        let path = path.clone();
        async move {
          let stream = tokio::net::UnixStream::connect(path).await?;
          Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
        }
      }))
      .await?,
  )
}

#[cfg(not(unix))]
async fn connect_unix(path: String) -> Result<Channel> {
  Err(Error::InvalidEndpoint(format!("unix:{path}")))
}

#[cfg(target_os = "linux")]
async fn connect_vsock(cid: u32, port: u32) -> Result<Channel> {
  Ok(
    placeholder()
      .connect_with_connector(tower::service_fn(move |_: Uri| async move {
        let stream = tokio_vsock::VsockStream::connect(tokio_vsock::VsockAddr::new(cid, port)).await?;
        Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
      }))
      .await?,
  )
}

#[cfg(not(target_os = "linux"))]
async fn connect_vsock(cid: u32, port: u32) -> Result<Channel> {
  Err(Error::InvalidEndpoint(format!("vsock://{cid}:{port} needs Linux")))
}
