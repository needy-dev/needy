//! The executor instance pool: instances by key, their reference counts, and
//! their slots.
//!
//! Runs whose executor key renders equal share one instance. The pool
//! creates an instance through its executor driver on the first
//! [`Pool::acquire`] and destroys it when the last holder releases it. An
//! instance with `max_procs` has that many slots; a command takes one for as
//! long as it runs, and `on_full` decides what a command does when none is
//! free.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context, Result, bail};
use needy_api_server::control::ControlError;
use needy_config::profile::{self, OnFull};
use needy_core_executor::driver::{self, Driver, LinuxContainer, Native, Spec, sftp_command};
use tokio::sync::{OnceCell, OwnedSemaphorePermit, Semaphore};

use crate::link::{Links, RunnerExecutor};

/// One executor instance and what the daemon knows about it.
pub struct Instance {
  /// The executor's name in the profile.
  pub name: String,
  /// The pool key; empty for this machine's own instance.
  pub key: String,
  pub handle: driver::Instance,
  /// The driver that created the instance and destroys it; `None` for a
  /// runner's machine, which nothing creates.
  driver: Option<Arc<dyn Driver>>,
  /// Actor path prefix to executor path prefix.
  pub path_map: BTreeMap<String, String>,
  slots: Option<Arc<Semaphore>>,
  on_full: OnFull,
  refs: AtomicUsize,
}

/// A command's hold on an instance's slot; dropping it frees the slot.
pub type Slot = Option<OwnedSemaphorePermit>;

impl Instance {
  /// This machine, through the daemon's in-process executor. It is never
  /// pooled and never destroyed.
  pub fn machine(native: &Arc<Native>) -> Arc<Self> {
    Arc::new(Self {
      name: "local".into(),
      key: String::new(),
      handle: native.machine(),
      driver: Some(native.clone()),
      path_map: BTreeMap::new(),
      slots: None,
      on_full: OnFull::Wait,
      refs: AtomicUsize::new(1),
    })
  }

  /// A connected runner's own machine, for commands outside any run
  /// (`needy exec --runner`). It is never pooled and never destroyed.
  pub fn runner(runner: &str, links: &Arc<Links>) -> Arc<Self> {
    Arc::new(Self {
      name: runner.to_string(),
      key: String::new(),
      // No run mounts a workspace from it.
      handle: runner_handle(runner, links, Vec::new(), String::new()),
      driver: None,
      path_map: BTreeMap::new(),
      slots: None,
      on_full: OnFull::Wait,
      refs: AtomicUsize::new(1),
    })
  }

  /// The process service a run's actor serves for its driving program on a
  /// terminal. It is never pooled; the actor's VM ends it.
  pub fn actor(run: &str, executor: Arc<dyn needy_api_client::Executor>) -> Arc<Self> {
    Arc::new(Self {
      name: "actor".into(),
      key: String::new(),
      handle: driver::Instance {
        driver: "actor",
        id: run.to_string(),
        executor,
        sftp: Vec::new(),
        workspace: String::new(),
      },
      driver: None,
      path_map: BTreeMap::new(),
      slots: None,
      on_full: OnFull::Wait,
      refs: AtomicUsize::new(1),
    })
  }

  /// The runner whose own process space the instance is, which owns its
  /// processes; `None` on the control host.
  pub fn on_runner(&self) -> Option<&str> {
    (self.handle.driver == RUNNER).then_some(self.handle.id.as_str())
  }

  /// Takes a slot for one command. A full instance refuses the command with
  /// `on_full: reject` and makes it wait otherwise.
  pub async fn slot(&self) -> Result<Slot, ControlError> {
    let Some(slots) = &self.slots else {
      return Ok(None);
    };
    let permit = match self.on_full {
      OnFull::Reject => slots.clone().try_acquire_owned().map_err(|_| ControlError::Exhausted("executor is full".into()))?,
      OnFull::Wait => slots.clone().acquire_owned().await.map_err(|_| ControlError::Unavailable("executor closed".into()))?,
    };
    Ok(Some(permit))
  }
}

/// What a runner's own process space has for its driver: no driver creates it.
const RUNNER: &str = "runner";

/// A runner's own process space, reached over its streams.
fn runner_handle(runner: &str, links: &Arc<Links>, sftp: Vec<String>, workspace: String) -> driver::Instance {
  let executor = RunnerExecutor {
    runner: runner.to_string(),
    links: links.clone(),
  };
  driver::Instance {
    driver: RUNNER,
    id: runner.to_string(),
    executor: Arc::new(executor),
    sftp,
    workspace,
  }
}

/// Where the pool creates an instance, as the scheduler placed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Site {
  /// This host: the daemon's own process space or its engine. A container
  /// executor on the control host's own runner is created here too.
  ControlHost,
  /// A runner's own process space, reached over its streams; `os` is its
  /// operating system as it reported it, which picks the SFTP server of a
  /// workspace mount.
  Runner { name: String, os: String },
}

/// One key's instance: created once by the first acquire, which the others
/// of that key wait for.
type KeyCell = Arc<OnceCell<Arc<Instance>>>;

/// The instances of every run, by key, and the drivers that create
/// them.
pub struct Pool {
  /// The lock covers only the map and the reference counts; instances are
  /// created outside it.
  instances: Mutex<HashMap<String, KeyCell>>,
  native: Arc<Native>,
  /// `None` when the Docker client could not be set up (a bad `DOCKER_HOST`).
  container: Option<Arc<LinuxContainer>>,
  /// The runners whose control streams are open, for instances on them.
  links: Arc<Links>,
}

impl Pool {
  pub fn new(native: Arc<Native>, container: Option<LinuxContainer>, links: Arc<Links>) -> Self {
    Self {
      instances: Mutex::new(HashMap::new()),
      native,
      container: container.map(Arc::new),
      links,
    }
  }

  fn lock(&self) -> MutexGuard<'_, HashMap<String, KeyCell>> {
    // NOTICE: every change under the lock is one map or counter update, so a
    // panic elsewhere leaves the map whole.
    self.instances.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }

  /// The instance for the spec's key at `site`, created through the
  /// executor's driver when no run holds one yet. Acquires of one key
  /// share one creation; a failed creation leaves the key free for the next
  /// acquire.
  pub async fn acquire(&self, spec: Spec, site: &Site) -> Result<Arc<Instance>> {
    loop {
      let cell = self.lock().entry(spec.key.clone()).or_default().clone();
      let created = cell.get_or_try_init(|| self.create(&spec, site)).await;
      let mut instances = self.lock();
      let current = instances.get(&spec.key).is_some_and(|c| Arc::ptr_eq(c, &cell));
      match created {
        Ok(instance) if current => {
          instance.refs.fetch_add(1, Ordering::SeqCst);
          return Ok(instance.clone());
        }
        // The last holder released the instance after it was created and
        // before this acquire counted itself; create another.
        Ok(_) => continue,
        Err(e) => {
          if current && !cell.initialized() {
            instances.remove(&spec.key);
          }
          return Err(e);
        }
      }
    }
  }

  /// Creates the instance of `spec` at `site` through its driver.
  async fn create(&self, spec: &Spec, site: &Site) -> Result<Arc<Instance>> {
    let (handle, driver): (driver::Instance, Option<Arc<dyn Driver>>) = match (spec.config.driver, site) {
      (profile::ExecutorDriver::Native, Site::ControlHost) => (self.native.create(spec).await?, Some(self.native.clone())),
      (profile::ExecutorDriver::LinuxContainer, Site::ControlHost) => {
        let container = self.container.clone().context("no Docker-compatible engine is available on this machine")?;
        (container.create(spec).await?, Some(container))
      }
      // The runner's machine outlives its runs; nothing creates or
      // destroys it. Its workspace is a path on that machine, as written.
      (profile::ExecutorDriver::Native, Site::Runner { name, os }) => {
        let workspace = spec.config.workspace.clone().unwrap_or_default();
        (runner_handle(name, &self.links, sftp_command(os), workspace), None)
      }
      // TODO(runner-docker): a runner creating containers itself (create and
      // destroy tasks on its control stream, execs relayed to the
      // container) is deferred; the scheduler places linux-container executors on
      // the control host, including those on its own runner, and refuses
      // other runners before this point.
      (profile::ExecutorDriver::LinuxContainer, Site::Runner { name, .. }) => {
        bail!("runner {name} cannot create linux-container executors yet")
      }
    };
    Ok(Arc::new(Instance {
      name: spec.name.clone(),
      key: spec.key.clone(),
      handle,
      driver,
      path_map: spec.config.path_map.clone(),
      slots: spec.config.max_procs.map(|n| Arc::new(Semaphore::new(n as usize))),
      on_full: spec.config.on_full,
      // Each acquire counts itself under the pool's lock.
      refs: AtomicUsize::new(0),
    }))
  }

  /// Gives back one hold on `instance`; the last one destroys it.
  pub async fn release(&self, instance: &Arc<Instance>) {
    {
      let mut instances = self.lock();
      if instance.refs.fetch_sub(1, Ordering::SeqCst) > 1 {
        return;
      }
      instances.retain(|_, cell| !cell.get().is_some_and(|v| Arc::ptr_eq(v, instance)));
    }
    if let Some(driver) = &instance.driver
      && let Err(e) = driver.destroy(&instance.handle).await
    {
      tracing::warn!("executor {}: cannot destroy {}: {e}", instance.name, instance.handle.id);
    }
  }

  /// The control host's `linux-container` driver, when it could be set up.
  pub fn container(&self) -> Option<&Arc<LinuxContainer>> {
    self.container.as_ref()
  }

  /// Every pooled instance.
  pub async fn instances(&self) -> Vec<Arc<Instance>> {
    self.lock().values().filter_map(|cell| cell.get().cloned()).collect()
  }

  /// The one pooled instance of the executor `name`, for a command outside
  /// any run. Instances of the same executor under different keys make
  /// the name ambiguous.
  pub async fn named(&self, name: &str) -> Result<Arc<Instance>, ControlError> {
    let instances = self.lock();
    let mut matches: Vec<_> = instances.values().filter_map(|cell| cell.get()).filter(|i| i.name == name).collect();
    match matches.len() {
      0 => Err(ControlError::NotFound("no such executor".into())),
      1 => Ok(matches[0].clone()),
      n => {
        matches.sort_by(|a, b| a.key.cmp(&b.key));
        let keys: Vec<&str> = matches.iter().map(|i| i.key.as_str()).collect();
        Err(ControlError::Invalid(format!("executor {name} is ambiguous: {n} instances ({}); name the run instead", keys.join(", "))))
      }
    }
  }
}

#[cfg(test)]
#[path = "pool_test.rs"]
mod tests;
