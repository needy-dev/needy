//! Client for the Needy daemon API.
//!
//! [`Client`] owns one gRPC channel and hands out a typed client per service.
//! Endpoints are `http://`, `https://`, `unix:/path`, `exec:COMMAND` (a
//! command whose standard input and output carry the connection, as ssh's
//! `ProxyCommand`) and, inside a Linux guest, `vsock://CID:PORT`. Every call
//! carries the bearer token when the caller sets one; the library never reads
//! a token from the environment.
//!
//! - [`Executor`]: what serves `ProcessService` to a caller: a daemon over
//!   gRPC ([`Client`]), or a runner over the stream it dialed.
//! - [`ready`]: connect and wait until the server answers its health check.
//! - [`exec`]: exec streams: start or attach, input, events, run to
//!   completion.
//! - [`files`]: tar archives over exec streams, to and from an executor.
//! - [`error`]: errors, with the server's status mapped to a [`Kind`].

mod command;
pub mod error;
pub mod exec;
pub mod files;
pub mod ready;

use std::pin::Pin;
use std::time::Duration;

use needy_api_proto::v1::client_service_client::ClientServiceClient;
use needy_api_proto::v1::guest_service_client::GuestServiceClient;
use needy_api_proto::v1::health_service_client::HealthServiceClient;
use needy_api_proto::v1::process_service_client::ProcessServiceClient;
use needy_api_proto::v1::run_service_client::RunServiceClient;
use needy_api_proto::v1::runner_service_client::RunnerServiceClient;
use needy_api_proto::v1::{ExecRequest, ExecResponse, KillProcessRequest, ListProcessesRequest, ListProcessesResponse};
use tokio::sync::mpsc;
use tokio_stream::Stream;
use tokio_stream::wrappers::ReceiverStream;
use tonic::metadata::{AsciiMetadataValue, MetadataValue};
use tonic::service::Interceptor;
use tonic::service::interceptor::InterceptedService;
#[cfg(unix)]
use tonic::transport::Uri;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};
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

// NOTICE: HTTP/2 pings every 20 s, answered within 20 s, find a connection
// that died without a reset (a sleeping laptop, a dropped tunnel) in under a
// minute; the server pings at the same interval.
const KEEP_ALIVE: Duration = Duration::from_secs(20);

impl Client {
  /// Connects to a daemon endpoint. HTTPS uses the platform's root
  /// certificates. A malformed endpoint, or one this platform cannot reach
  /// (vsock outside Linux), is [`Error::InvalidEndpoint`].
  pub async fn connect(url: &str) -> Result<Self> {
    Self::connect_with(url, false).await
  }

  /// [`Client::connect`] for long-lived streams, such as a runner's control
  /// stream: the connection is pinged while idle, so one that died silently
  /// fails its streams instead of leaving them open forever.
  pub async fn connect_kept_alive(url: &str) -> Result<Self> {
    Self::connect_with(url, true).await
  }

  async fn connect_with(url: &str, keep_alive: bool) -> Result<Self> {
    let configure = move |endpoint: Endpoint| {
      if keep_alive {
        endpoint.http2_keep_alive_interval(KEEP_ALIVE).keep_alive_timeout(KEEP_ALIVE).keep_alive_while_idle(true)
      } else {
        endpoint
      }
    };
    if let Some(path) = url.strip_prefix("unix:") {
      return Ok(Self::from_channel(connect_unix(configure(placeholder()), path.to_string()).await?));
    }
    if let Some(command) = url.strip_prefix("exec:") {
      let argv = shell_words::split(command).map_err(|_| Error::InvalidEndpoint(url.to_string()))?;
      if argv.is_empty() {
        return Err(Error::InvalidEndpoint(url.to_string()));
      }
      return Ok(Self::from_channel(command::connect(configure(placeholder()), argv).await?));
    }
    if let Some(rest) = url.strip_prefix("vsock://") {
      let (cid, port) = rest
        .split_once(':')
        .and_then(|(c, p)| Some((c.parse::<u32>().ok()?, p.parse::<u32>().ok()?)))
        .ok_or_else(|| Error::InvalidEndpoint(url.to_string()))?;
      return Ok(Self::from_channel(connect_vsock(configure(placeholder()), cid, port).await?));
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
      return Err(Error::InvalidEndpoint(url.to_string()));
    }
    let mut endpoint = Endpoint::from_shared(url.to_string()).map_err(|_| Error::InvalidEndpoint(url.to_string()))?;
    if url.starts_with("https://") {
      endpoint = endpoint.tls_config(ClientTlsConfig::new().with_native_roots())?;
    }
    Ok(Self::from_channel(configure(endpoint).connect().await?))
  }

  /// Connects to a guest's vsock `port` through the host side of a
  /// Firecracker vsock device, the Unix socket `path`. A port no one in the
  /// guest listens on fails.
  pub async fn connect_firecracker_vsock(path: &std::path::Path, port: u32) -> Result<Self> {
    Ok(Self::from_channel(connect_firecracker_vsock(placeholder(), path.to_path_buf(), port).await?))
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

  pub fn clients(&self) -> ClientServiceClient<Service> {
    ClientServiceClient::new(self.service())
  }

  pub fn guest(&self) -> GuestServiceClient<Service> {
    GuestServiceClient::new(self.service())
  }
}

/// An exec stream's responses, as the executor sends them.
pub type Responses = Pin<Box<dyn Stream<Item = Result<ExecResponse, Status>> + Send>>;

/// What serves `ProcessService` to a caller: a daemon reached over gRPC
/// ([`Client`]), or a runner that dialed the control plane, which reaches it
/// over the runner's own streams. Calls fail as `ProcessService` would, with
/// its statuses.
#[async_trait::async_trait]
pub trait Executor: Send + Sync {
  /// Opens an exec stream; the first request starts or attaches. Returns
  /// once the executor accepted it, or with the status it refused it with.
  async fn exec(&self, requests: mpsc::Receiver<ExecRequest>) -> Result<Responses, Status>;
  async fn list_processes(&self, request: ListProcessesRequest) -> Result<ListProcessesResponse, Status>;
  async fn kill_process(&self, request: KillProcessRequest) -> Result<(), Status>;
}

#[async_trait::async_trait]
impl Executor for Client {
  async fn exec(&self, requests: mpsc::Receiver<ExecRequest>) -> Result<Responses, Status> {
    let responses = self.processes().exec(ReceiverStream::new(requests)).await?.into_inner();
    Ok(Box::pin(responses))
  }

  async fn list_processes(&self, request: ListProcessesRequest) -> Result<ListProcessesResponse, Status> {
    Ok(self.processes().list_processes(request).await?.into_inner())
  }

  async fn kill_process(&self, request: KillProcessRequest) -> Result<(), Status> {
    self.processes().kill_process(request).await?;
    Ok(())
  }
}

fn placeholder() -> Endpoint {
  // The URI is ignored by custom connectors but must parse.
  Endpoint::from_static("http://needy.local")
}

#[cfg(unix)]
async fn connect_unix(endpoint: Endpoint, path: String) -> Result<Channel> {
  Ok(
    endpoint
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

/// Firecracker's host-initiated vsock: the host writes `CONNECT <port>` on
/// the device's Unix socket and reads `OK <host port>` before the stream
/// carries the connection.
// Source: https://github.com/firecracker-microvm/firecracker/blob/95f868c8e345b1cc8faccd1a3c910b4989dc3f58/docs/vsock.md#L46-L67
// Reference: https://github.com/firecracker-microvm/firecracker/blob/95f868c8e345b1cc8faccd1a3c910b4989dc3f58/docs/vsock.md#host-initiated-connections
#[cfg(unix)]
async fn connect_firecracker_vsock(endpoint: Endpoint, path: std::path::PathBuf, port: u32) -> Result<Channel> {
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  Ok(
    endpoint
      .connect_with_connector(tower::service_fn(move |_: Uri| {
        let path = path.clone();
        async move {
          let mut stream = tokio::net::UnixStream::connect(path).await?;
          stream.write_all(format!("CONNECT {port}\n").as_bytes()).await?;
          // Read the answer byte by byte: what follows its newline belongs
          // to the connection.
          let mut answer = Vec::new();
          loop {
            let byte = stream.read_u8().await?;
            if byte == b'\n' {
              break;
            }
            answer.push(byte);
            if answer.len() > 64 {
              break;
            }
          }
          if !answer.starts_with(b"OK ") {
            let answer = String::from_utf8_lossy(&answer).into_owned();
            return Err(std::io::Error::new(std::io::ErrorKind::ConnectionRefused, format!("vsock port {port}: {answer:?}")));
          }
          Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
        }
      }))
      .await?,
  )
}

#[cfg(not(unix))]
async fn connect_firecracker_vsock(_endpoint: Endpoint, path: std::path::PathBuf, port: u32) -> Result<Channel> {
  Err(Error::InvalidEndpoint(format!("{}:{port} needs Unix", path.display())))
}

#[cfg(not(unix))]
async fn connect_unix(_endpoint: Endpoint, path: String) -> Result<Channel> {
  Err(Error::InvalidEndpoint(format!("unix:{path}")))
}

#[cfg(target_os = "linux")]
async fn connect_vsock(endpoint: Endpoint, cid: u32, port: u32) -> Result<Channel> {
  Ok(
    endpoint
      .connect_with_connector(tower::service_fn(move |_: Uri| async move {
        let stream = tokio_vsock::VsockStream::connect(tokio_vsock::VsockAddr::new(cid, port)).await?;
        Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
      }))
      .await?,
  )
}

#[cfg(not(target_os = "linux"))]
async fn connect_vsock(_endpoint: Endpoint, cid: u32, port: u32) -> Result<Channel> {
  Err(Error::InvalidEndpoint(format!("vsock://{cid}:{port} needs Linux")))
}
