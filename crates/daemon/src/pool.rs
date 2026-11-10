//! The executor instance pool: instances by key, their reference counts, and
//! their slots.
//!
//! Runs whose executor key renders equal share one instance. The pool
//! creates an instance through its runtime's provider on the first
//! [`Pool::acquire`] and destroys it when the last holder releases it. An
//! instance with `max_procs` has that many slots; a command takes one for as
//! long as it runs, and `on_full` decides what a command does when none is
//! free.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context, Result, bail};
use needy_api_server::control::ControlError;
use needy_config::profile::{OnFull, Runtime};
use needy_core_executor::provider::{Docker, InstanceHandle, InstanceSpec, Native, Provider, Remote};
use tokio::sync::{OnceCell, OwnedSemaphorePermit, Semaphore};

use crate::link::{Links, RunnerExecutor};

/// One executor instance and what the daemon knows about it.
pub struct Instance {
  /// The executor's name in the profile.
  pub name: String,
  /// The pool key; empty for this machine's own instance.
  pub key: String,
  pub handle: InstanceHandle,
  /// The provider that created the instance and destroys it; `None` for a
  /// runner's machine, which nothing creates.
  provider: Option<Arc<dyn Provider>>,
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
      provider: Some(native.clone()),
      path_map: BTreeMap::new(),
      slots: None,
      on_full: OnFull::Wait,
      refs: AtomicUsize::new(1),
    })
  }

  /// A connected runner's own machine, for commands outside any run
  /// (`needy exec --runner`). It is never pooled and never destroyed.
  // TODO(runner-select): profiles cannot place executors on runners yet;
  // `runner:` selectors in profiles are slice 5 of the runners-and-runs
  // design, which pools runner instances by key like other runtimes.
  pub fn runner(runner: &str, links: &Arc<Links>) -> Arc<Self> {
    let executor = RunnerExecutor {
      runner: runner.to_string(),
      links: links.clone(),
    };
    Arc::new(Self {
      name: runner.to_string(),
      key: String::new(),
      handle: InstanceHandle {
        runtime: "runner",
        id: runner.to_string(),
        executor: Arc::new(executor),
        // TODO(runner-select): a workspace mount on a runner needs its
        // platform's SFTP server; mounts reach runners with slice 5.
        sftp: Vec::new(),
        workspace: String::new(),
      },
      provider: None,
      path_map: BTreeMap::new(),
      slots: None,
      on_full: OnFull::Wait,
      refs: AtomicUsize::new(1),
    })
  }

  /// Takes a slot for one command. A full instance refuses the command with
  /// `on_full: reject` and makes it wait otherwise.
  // TODO(executor-pool): `on_full: new` waits like `wait`; a second instance
  // per key needs `max_instances`, which is not verified yet (see the
  // closed-loop results' "Not yet verified").
  pub async fn slot(&self) -> Result<Slot, ControlError> {
    let Some(slots) = &self.slots else {
      return Ok(None);
    };
    let permit = match self.on_full {
      OnFull::Reject => slots.clone().try_acquire_owned().map_err(|_| ControlError::Exhausted("executor is full".into()))?,
      OnFull::Wait | OnFull::New => slots.clone().acquire_owned().await.map_err(|_| ControlError::Unavailable("executor closed".into()))?,
    };
    Ok(Some(permit))
  }
}

/// One key's instance: created once by the first acquire, which the others
/// of that key wait for.
type KeyCell = Arc<OnceCell<Arc<Instance>>>;

/// The instances of every run, by key, and the providers that create
/// them.
pub struct Pool {
  /// The lock covers only the map and the reference counts; instances are
  /// created outside it.
  instances: Mutex<HashMap<String, KeyCell>>,
  native: Arc<Native>,
  remote: Arc<Remote>,
  /// `None` when the Docker client could not be set up (a bad `DOCKER_HOST`).
  docker: Option<Arc<Docker>>,
}

impl Pool {
  pub fn new(native: Arc<Native>, docker: Option<Docker>) -> Self {
    Self {
      instances: Mutex::new(HashMap::new()),
      native,
      remote: Arc::new(Remote),
      docker: docker.map(Arc::new),
    }
  }

  fn lock(&self) -> MutexGuard<'_, HashMap<String, KeyCell>> {
    // NOTICE: every change under the lock is one map or counter update, so a
    // panic elsewhere leaves the map whole.
    self.instances.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }

  /// The instance for the spec's key, created through the executor's
  /// provider when no run holds one yet. Acquires of one key share one
  /// creation; a failed creation leaves the key free for the next acquire.
  pub async fn acquire(&self, spec: InstanceSpec) -> Result<Arc<Instance>> {
    loop {
      let cell = self.lock().entry(spec.key.clone()).or_default().clone();
      let created = cell.get_or_try_init(|| self.create(&spec)).await;
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

  /// Creates the instance of `spec` through its runtime's provider.
  async fn create(&self, spec: &InstanceSpec) -> Result<Arc<Instance>> {
    let provider: Arc<dyn Provider> = match spec.config.runtime {
      Runtime::Native => self.native.clone(),
      // TODO(runner-select): replaced by runner executors once profiles
      // select runners (slice 5); until then a profile reaches another
      // machine through an executor daemon it dials.
      Runtime::Remote => self.remote.clone(),
      Runtime::Docker => self.docker.clone().context("Docker is not available on this machine")?,
      // TODO(provider): containerd, firecracker and k8s providers are
      // deferred; see `needy_core_executor::provider`.
      other => bail!("runtime {other:?} is not supported yet"),
    };
    let handle = provider.create(spec).await?;
    Ok(Arc::new(Instance {
      name: spec.name.clone(),
      key: spec.key.clone(),
      handle,
      provider: Some(provider),
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
    if let Some(provider) = &instance.provider
      && let Err(e) = provider.destroy(&instance.handle).await
    {
      tracing::warn!("executor {}: cannot destroy {}: {e}", instance.name, instance.handle.id);
    }
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
