//! The Docker-compatible container engine both `linux-container` drivers
//! talk to: the executor driver here and the actor driver in
//! `needy-core-actor`. One client, through the Docker Engine API
//! (`bollard`), reaching whichever engine `DOCKER_HOST` names, else the
//! default socket.
//!
//! Docker Engine, OrbStack, Podman and containerd (through its
//! Docker-compatible tooling) are equivalent engines here (owner decision,
//! 2026-11-12): there is no per-engine code.
//!
//! [`Engine`] is the boundary drivers use and tests stand in for; [`local`]
//! is this machine's engine, or an [`Unreachable`] one that fails each use
//! with the reason. [`Probe`] answers whether an engine answers at all, once
//! for every driver that asks.
//!
//! See `docs/ai/references/architecture/2026-11-12-drivers-and-extension-points-design.md`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use bollard::errors::Error as BollardError;
use bollard::query_parameters::{
  CreateContainerOptionsBuilder, CreateImageOptionsBuilder, LogsOptionsBuilder, RemoveContainerOptionsBuilder, StopContainerOptionsBuilder,
  WaitContainerOptionsBuilder,
};
use futures_util::stream::BoxStream;
use futures_util::{StreamExt, TryStreamExt};

/// The engine API's types a driver builds a container from.
pub use bollard::models;

use models::ContainerCreateBody;

/// The label with the run a container belongs to.
pub const RUN_LABEL: &str = "needy.run";
/// The label with what a container is to its run: `actor` or `executor`.
pub const ROLE_LABEL: &str = "needy.role";

/// The labels of a container of `run` that is its `role` (`actor`,
/// `executor`).
// TODO(engine): no one lists containers by these labels yet; a restarted
// daemon finds its leftovers by the names `state.db` has. Listing by label
// would also find containers whose rows are gone, which changes what
// reconcile ends, so it needs its own approved slice.
pub fn labels(run: &str, role: &str) -> HashMap<String, String> {
  HashMap::from([
    (RUN_LABEL.to_string(), run.to_string()),
    (ROLE_LABEL.to_string(), role.to_string()),
  ])
}

/// What the engine did not do. The messages say it whole, for a driver to
/// wrap in its own error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
  /// No engine answers where `DOCKER_HOST` points (or at the default
  /// socket), with the reason.
  #[error("no Docker-compatible engine answers: {0}")]
  Unreachable(String),
  /// The image or container named does not exist (the engine's 404).
  #[error("docker: {0}")]
  NotFound(String),
  /// What the engine refused or failed otherwise.
  #[error("docker: {0}")]
  Refused(String),
}

impl Error {
  pub fn is_not_found(&self) -> bool {
    matches!(self, Self::NotFound(_))
  }
}

impl From<BollardError> for Error {
  fn from(e: BollardError) -> Self {
    match e {
      BollardError::DockerResponseServerError {
        status_code: 404, ..
      } => Self::NotFound(e.to_string()),
      e => Self::Refused(e.to_string()),
    }
  }
}

/// The engine, as far as needy uses it. A real boundary (a container
/// runtime), so tests stand in for it.
#[async_trait::async_trait]
pub trait Engine: Send + Sync {
  /// The OCI runtimes it has registered, each with the arguments it passes
  /// that runtime (`runtimeArgs`). It answers only if the engine does, so
  /// it is also the probe of whether one does.
  async fn runtimes(&self) -> Result<BTreeMap<String, Vec<String>>, Error>;
  /// The labels of `image`, or `None` when the engine does not have it.
  async fn image_labels(&self, image: &str) -> Result<Option<HashMap<String, String>>, Error>;
  /// Pulls `image`; a bare name means its `latest` tag.
  async fn pull(&self, image: &str) -> Result<(), Error>;
  /// Creates the container `name`; [`Error::NotFound`] when its image is
  /// not here.
  async fn create(&self, name: &str, body: ContainerCreateBody) -> Result<(), Error>;
  /// Starts the container and returns its first process's host PID.
  async fn start(&self, name: &str) -> Result<u32, Error>;
  /// The container's stdout and stderr from its start, until it exits.
  fn logs(&self, name: &str) -> BoxStream<'static, Result<bytes::Bytes, Error>>;
  /// Waits until the container exits and returns its exit code.
  async fn wait(&self, name: &str) -> Result<i64, Error>;
  /// `SIGTERM` to its first process, `SIGKILL` once `grace` has passed. One
  /// that is not running is fine.
  async fn stop(&self, name: &str, grace: Duration) -> Result<(), Error>;
  /// Removes the container, running or not; one that is gone already, or
  /// whose removal is in progress, is fine.
  async fn remove(&self, name: &str) -> Result<(), Error>;
}

/// This machine's engine (`DOCKER_HOST`, else the default socket), or the
/// reason it cannot be reached. Nothing is contacted until it is used.
pub fn connect() -> Result<Arc<dyn Engine>, Error> {
  bollard::Docker::connect_with_defaults()
    .map(|docker| Arc::new(Docker(docker)) as Arc<dyn Engine>)
    .map_err(|e| Error::Unreachable(e.to_string()))
}

/// This machine's engine, as [`connect`] finds it, or an [`Unreachable`]
/// one that fails each use with the reason.
pub fn local() -> Arc<dyn Engine> {
  connect().unwrap_or_else(|e| match e {
    Error::Unreachable(why) => Arc::new(Unreachable(why)),
    e => Arc::new(Unreachable(e.to_string())),
  })
}

// NOTICE: how long detection waits for the engine. A local socket answers in
// milliseconds; a Docker Desktop that is not running fails at once. 3 s
// bounds an engine that hangs without delaying a runner's join much.
const DETECT_WAIT: Duration = Duration::from_secs(3);

/// Whether this machine's engine answers within [`DETECT_WAIT`], asked once
/// and shared by every driver that detects through the engine (both
/// `linux-container` drivers), so a hanging engine costs a runner's start
/// one wait, not one per driver.
#[derive(Default)]
pub struct Probe(tokio::sync::OnceCell<Result<(), String>>);

impl Probe {
  /// The answer, asking the engine the first time.
  pub async fn answer(&self) -> Result<(), String> {
    self.0.get_or_init(|| detect(local())).await.clone()
  }
}

/// Whether `engine` answers within [`DETECT_WAIT`].
async fn detect(engine: Arc<dyn Engine>) -> Result<(), String> {
  match tokio::time::timeout(DETECT_WAIT, engine.runtimes()).await {
    Ok(runtimes) => runtimes.map(drop).map_err(|e| e.to_string()),
    Err(_) => Err(format!("no Docker-compatible engine answered within {DETECT_WAIT:?}")),
  }
}

/// An engine that cannot be reached, and why.
pub struct Unreachable(pub String);

impl Unreachable {
  fn error(&self) -> Error {
    Error::Unreachable(self.0.clone())
  }
}

#[async_trait::async_trait]
impl Engine for Unreachable {
  async fn runtimes(&self) -> Result<BTreeMap<String, Vec<String>>, Error> {
    Err(self.error())
  }

  async fn image_labels(&self, _image: &str) -> Result<Option<HashMap<String, String>>, Error> {
    Err(self.error())
  }

  async fn pull(&self, _image: &str) -> Result<(), Error> {
    Err(self.error())
  }

  async fn create(&self, _name: &str, _body: ContainerCreateBody) -> Result<(), Error> {
    Err(self.error())
  }

  async fn start(&self, _name: &str) -> Result<u32, Error> {
    Err(self.error())
  }

  fn logs(&self, _name: &str) -> BoxStream<'static, Result<bytes::Bytes, Error>> {
    futures_util::stream::iter([Err(self.error())]).boxed()
  }

  async fn wait(&self, _name: &str) -> Result<i64, Error> {
    Err(self.error())
  }

  async fn stop(&self, _name: &str, _grace: Duration) -> Result<(), Error> {
    Err(self.error())
  }

  async fn remove(&self, _name: &str) -> Result<(), Error> {
    Err(self.error())
  }
}

/// The engine through `bollard`.
struct Docker(bollard::Docker);

#[async_trait::async_trait]
impl Engine for Docker {
  async fn runtimes(&self) -> Result<BTreeMap<String, Vec<String>>, Error> {
    // Any failure here means no engine answers: `info` asks nothing that
    // an engine refuses.
    let info = self.0.info().await.map_err(|e| Error::Unreachable(e.to_string()))?;
    Ok(info.runtimes.unwrap_or_default().into_iter().map(|(name, runtime)| (name, runtime.runtime_args.unwrap_or_default())).collect())
  }

  async fn image_labels(&self, image: &str) -> Result<Option<HashMap<String, String>>, Error> {
    match self.0.inspect_image(image).await.map_err(Error::from) {
      Ok(inspected) => Ok(Some(inspected.config.and_then(|config| config.labels).unwrap_or_default())),
      Err(e) if e.is_not_found() => Ok(None),
      Err(e) => Err(e),
    }
  }

  async fn pull(&self, image: &str) -> Result<(), Error> {
    let options = CreateImageOptionsBuilder::default().from_image(&with_tag(image)).build();
    self.0.create_image(Some(options), None, None).try_collect::<Vec<_>>().await?;
    Ok(())
  }

  async fn create(&self, name: &str, body: ContainerCreateBody) -> Result<(), Error> {
    let options = CreateContainerOptionsBuilder::default().name(name).build();
    self.0.create_container(Some(options), body).await?;
    Ok(())
  }

  async fn start(&self, name: &str) -> Result<u32, Error> {
    self.0.start_container(name, None).await?;
    let inspected = self.0.inspect_container(name, None).await?;
    Ok(inspected.state.and_then(|s| s.pid).and_then(|pid| u32::try_from(pid).ok()).unwrap_or(0))
  }

  fn logs(&self, name: &str) -> BoxStream<'static, Result<bytes::Bytes, Error>> {
    let options = LogsOptionsBuilder::default().follow(true).stdout(true).stderr(true).build();
    self.0.logs(name, Some(options)).map(|output| output.map(|o| o.into_bytes()).map_err(Error::from)).boxed()
  }

  async fn wait(&self, name: &str) -> Result<i64, Error> {
    let options = WaitContainerOptionsBuilder::default().condition("not-running").build();
    let mut waited = self.0.wait_container(name, Some(options));
    exit_code(name, waited.next().await)
  }

  async fn stop(&self, name: &str, grace: Duration) -> Result<(), Error> {
    let options = StopContainerOptionsBuilder::default().t(grace.as_secs() as i32).build();
    ignore(self.0.stop_container(name, Some(options)).await, &[304])
  }

  async fn remove(&self, name: &str) -> Result<(), Error> {
    let options = RemoveContainerOptionsBuilder::default().force(true).build();
    ignore(self.0.remove_container(name, Some(options)).await, &[404, 409])
  }
}

/// The exit code of a wait's first answer.
fn exit_code(name: &str, answer: Option<Result<models::ContainerWaitResponse, BollardError>>) -> Result<i64, Error> {
  match answer {
    Some(Ok(response)) => Ok(response.status_code),
    // NOTICE: bollard reports a non-zero exit as this error, with the code
    // (bollard 0.21.1 `src/container.rs`, `wait_container`).
    Some(Err(BollardError::DockerContainerWaitError { code, .. })) => Ok(code),
    Some(Err(e)) => Err(e.into()),
    None => Err(Error::Refused(format!("the wait for {name} ended without its exit"))),
  }
}

/// `result`, with the engine's answers of `fine` status codes as success:
/// 304 to a stop (it was not running), 404 to a removal (no such container)
/// and 409 to one (its removal is in progress, as with `--rm`).
fn ignore(result: Result<(), BollardError>, fine: &[u16]) -> Result<(), Error> {
  match result {
    Ok(()) => Ok(()),
    Err(BollardError::DockerResponseServerError { status_code, .. }) if fine.contains(&status_code) => Ok(()),
    Err(e) => Err(e.into()),
  }
}

/// An image reference with its tag: the API pulls every tag of a bare name.
fn with_tag(image: &str) -> String {
  let last = image.rsplit('/').next().unwrap_or(image);
  if last.contains(':') || last.contains('@') {
    image.to_string()
  } else {
    format!("{image}:latest")
  }
}

#[cfg(test)]
#[path = "engine_test.rs"]
mod tests;
