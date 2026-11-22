//! Shared fake [`Engine`] test double for unit and integration tests across
//! `needy-core-executor`, `needy-core-actor`, and `needy-daemon`.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt as _;
use futures_util::stream::BoxStream;

use super::models::{ContainerCreateBody, CreateImageInfo};
use super::{Engine, Error, Image};

/// Boxed future returned by optional [`Fake`] lifecycle hooks.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
/// Hook invoked when [`Engine::start`] runs on [`Fake`].
pub type StartHook = Arc<dyn Fn(&str, Option<ContainerCreateBody>) -> BoxFuture<Result<u32, Error>> + Send + Sync>;
/// Hook invoked when [`Engine::remove`] runs on [`Fake`].
pub type RemoveHook = Arc<dyn Fn(&str) -> BoxFuture<Result<(), Error>> + Send + Sync>;

/// Configurable in-memory [`Engine`] test double.
///
/// Records every call (`pull`, `create`, `start`, `wait`, `stop`, `remove`,
/// `download`, `commit`, `export`, `load`, `remove_image`) and every created
/// container's `(name, ContainerCreateBody)`.
pub struct Fake {
  calls: Mutex<Vec<String>>,
  bodies: Mutex<Vec<(String, ContainerCreateBody)>>,
  runtimes: BTreeMap<String, Vec<String>>,
  /// Images already present in the engine before any pull.
  images: HashMap<String, Image>,
  /// Registry images available to `pull_progress`. `None` means any pull
  /// succeeds and installs a default [`Image`]; `Some(map)` fails with
  /// [`Error::NotFound`] for images absent from `map`.
  registry: Option<HashMap<String, Image>>,
  /// Images pulled so far.
  pulled: Mutex<HashMap<String, Image>>,
  /// When true, `create` returns [`Error::NotFound`] until the requested
  /// image exists in `images` or `pulled` (or any pull has occurred if the
  /// container body has no image set).
  require_image_on_create: bool,
  /// Files served by `download` as single-file GNU tar archives.
  files: HashMap<String, String>,
  /// Default result of `start` when no `on_start` hook is installed.
  start_result: Result<u32, Error>,
  /// Result of `wait`.
  wait_result: Result<i64, Error>,
  /// Log output yielded by `logs`.
  log: String,
  /// When true, `stop` delegates to `remove` (for fixtures whose containers
  /// have no separate stop phase).
  stop_removes: bool,
  on_start: Option<StartHook>,
  on_remove: Option<RemoveHook>,
}

impl Default for Fake {
  fn default() -> Self {
    Self {
      calls: Mutex::new(Vec::new()),
      bodies: Mutex::new(Vec::new()),
      runtimes: BTreeMap::new(),
      images: HashMap::new(),
      registry: None,
      pulled: Mutex::new(HashMap::new()),
      require_image_on_create: false,
      files: HashMap::new(),
      start_result: Ok(4242),
      wait_result: Ok(0),
      log: String::new(),
      stop_removes: false,
      on_start: None,
      on_remove: None,
    }
  }
}

impl Fake {
  /// A fresh fake engine with no images, no runtimes, `start` returning PID
  /// 4242, and `wait` returning exit code 0.
  pub fn new() -> Self {
    Self::default()
  }

  /// Registers an OCI runtime with its `runtimeArgs`.
  pub fn with_runtime(mut self, name: impl Into<String>, args: &[&str]) -> Self {
    self.runtimes.insert(name.into(), args.iter().map(|a| (*a).to_string()).collect());
    self
  }

  /// Adds an image already present in the engine.
  pub fn with_image(mut self, name: impl Into<String>, image: Image) -> Self {
    self.images.insert(name.into(), image);
    self
  }

  /// Restricts pulls to an explicit registry map (pulls of images not in the
  /// map fail with [`Error::NotFound`]).
  pub fn with_registry(mut self, images: impl IntoIterator<Item = (String, Image)>) -> Self {
    self.registry = Some(images.into_iter().collect());
    self
  }

  /// Requires an image to be present (or pulled) before `create` succeeds.
  pub fn require_image_on_create(mut self) -> Self {
    self.require_image_on_create = true;
    self
  }

  /// Registers a file path served by `download` as a single-file tar archive.
  pub fn with_file(mut self, path: impl Into<String>, content: impl Into<String>) -> Self {
    self.files.insert(path.into(), content.into());
    self
  }

  /// Makes `start` fail with `error`.
  pub fn with_start_error(mut self, error: Error) -> Self {
    self.start_result = Err(error);
    self
  }

  /// Sets the exit code returned by `wait`.
  pub fn with_exit_code(mut self, code: i64) -> Self {
    self.wait_result = Ok(code);
    self
  }

  /// Makes `wait` fail with `error`.
  pub fn with_wait_error(mut self, error: Error) -> Self {
    self.wait_result = Err(error);
    self
  }

  /// Sets the log bytes yielded by `logs`.
  pub fn with_log(mut self, log: impl Into<String>) -> Self {
    self.log = log.into();
    self
  }

  /// Delegates `stop` to `remove`.
  pub fn with_stop_removes(mut self) -> Self {
    self.stop_removes = true;
    self
  }

  /// Installs a custom async hook invoked by `start`.
  pub fn with_on_start(mut self, hook: StartHook) -> Self {
    self.on_start = Some(hook);
    self
  }

  /// Installs a custom async hook invoked by `remove`.
  pub fn with_on_remove(mut self, hook: RemoveHook) -> Self {
    self.on_remove = Some(hook);
    self
  }

  /// Snapshot of all recorded engine calls in order.
  pub fn calls(&self) -> Vec<String> {
    self.calls.lock().unwrap().clone()
  }

  /// Snapshot of all `(container_name, ContainerCreateBody)` recorded by `create`.
  pub fn bodies(&self) -> Vec<(String, ContainerCreateBody)> {
    self.bodies.lock().unwrap().clone()
  }

  /// The last recorded [`ContainerCreateBody`] for `name`, if any.
  pub fn body(&self, name: &str) -> Option<ContainerCreateBody> {
    self.bodies.lock().unwrap().iter().rev().find(|(n, _)| n == name).map(|(_, body)| body.clone())
  }

  fn record(&self, call: String) {
    self.calls.lock().unwrap().push(call);
  }

  fn has_image(&self, image: &str) -> bool {
    self.images.contains_key(image) || self.pulled.lock().unwrap().contains_key(image)
  }
}

#[async_trait::async_trait]
impl Engine for Fake {
  async fn runtimes(&self) -> Result<BTreeMap<String, Vec<String>>, Error> {
    Ok(self.runtimes.clone())
  }

  async fn inspect_image(&self, image: &str) -> Result<Option<Image>, Error> {
    if let Some(found) = self.images.get(image) {
      return Ok(Some(found.clone()));
    }
    Ok(self.pulled.lock().unwrap().get(image).cloned())
  }

  fn pull_progress(&self, image: &str) -> BoxStream<'static, Result<CreateImageInfo, Error>> {
    self.record(format!("pull {image}"));
    match &self.registry {
      Some(registry) => match registry.get(image) {
        Some(found) => {
          self.pulled.lock().unwrap().insert(image.to_string(), found.clone());
          futures_util::stream::empty().boxed()
        }
        None => futures_util::stream::iter([Err(Error::NotFound(format!(
          "manifest for {image} not found: manifest unknown"
        )))])
        .boxed(),
      },
      None => {
        self.pulled.lock().unwrap().insert(image.to_string(), Image::default());
        futures_util::stream::empty().boxed()
      }
    }
  }

  async fn create(&self, name: &str, body: ContainerCreateBody) -> Result<(), Error> {
    self.record(format!("create {name}"));
    let image = body.image.clone();
    self.bodies.lock().unwrap().push((name.to_string(), body));
    if self.require_image_on_create {
      let available = match image.as_deref() {
        Some(img) => self.has_image(img),
        None => !self.pulled.lock().unwrap().is_empty(),
      };
      if !available {
        return Err(Error::NotFound("No such image".into()));
      }
    }
    Ok(())
  }

  async fn start(&self, name: &str) -> Result<u32, Error> {
    self.record(format!("start {name}"));
    if let Some(hook) = &self.on_start {
      let body = self.body(name);
      return hook(name, body).await;
    }
    self.start_result.clone()
  }

  fn logs(&self, _name: &str) -> BoxStream<'static, Result<bytes::Bytes, Error>> {
    if self.log.is_empty() {
      futures_util::stream::empty().boxed()
    } else {
      futures_util::stream::iter([Ok(bytes::Bytes::from(self.log.clone()))]).boxed()
    }
  }

  async fn wait(&self, name: &str) -> Result<i64, Error> {
    self.record(format!("wait {name}"));
    self.wait_result.clone()
  }

  async fn stop(&self, name: &str, grace: Duration) -> Result<(), Error> {
    self.record(format!("stop {name} {}s", grace.as_secs()));
    if self.stop_removes {
      return self.remove(name).await;
    }
    Ok(())
  }

  async fn remove(&self, name: &str) -> Result<(), Error> {
    self.record(format!("remove {name}"));
    if let Some(hook) = &self.on_remove {
      hook(name).await?;
    }
    Ok(())
  }

  async fn download(&self, name: &str, path: &str) -> Result<Option<Vec<u8>>, Error> {
    self.record(format!("download {name} {path}"));
    let Some(text) = self.files.get(path) else {
      return Ok(None);
    };
    let mut archive = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(text.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    archive.append_data(&mut header, path.rsplit('/').next().unwrap(), text.as_bytes()).unwrap();
    Ok(Some(archive.into_inner().unwrap()))
  }

  async fn commit_container(&self, container: &str, repo: &str, tag: &str) -> Result<String, Error> {
    self.record(format!("commit {container} {repo}:{tag}"));
    Ok(format!("sha256:committed-{container}"))
  }

  fn export_image(&self, image: &str) -> BoxStream<'static, Result<bytes::Bytes, Error>> {
    self.record(format!("export {image}"));
    futures_util::stream::empty().boxed()
  }

  async fn load_image(&self, mut tar: BoxStream<'static, Result<bytes::Bytes, Error>>) -> Result<(), Error> {
    while let Some(chunk) = tar.next().await {
      chunk?;
    }
    self.record("load".into());
    Ok(())
  }

  async fn list_images(&self, reference_filter: &str) -> Result<Vec<String>, Error> {
    self.record(format!("list_images {reference_filter}"));
    Ok(Vec::new())
  }

  async fn remove_image(&self, image: &str) -> Result<(), Error> {
    self.record(format!("remove_image {image}"));
    Ok(())
  }
}
