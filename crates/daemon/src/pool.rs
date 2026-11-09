//! The executor instance pool: instances by key, their reference counts, and
//! their slots.
//!
//! Sessions whose executor key renders equal share one instance. The pool
//! creates an instance through its runtime's provider on the first
//! [`Pool::acquire`] and destroys it when the last holder releases it. An
//! instance with `max_procs` has that many slots; a command takes one for as
//! long as it runs, and `on_full` decides what a command does when none is
//! free.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, bail};
use needy_api_server::control::ControlError;
use needy_config::profile::{OnFull, Runtime};
use needy_core_executor::provider::{Docker, InstanceHandle, InstanceSpec, Native, Provider, Remote};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// One executor instance and what the daemon knows about it.
pub struct Instance {
  /// The executor's name in the profile.
  pub name: String,
  /// The pool key; empty for this machine's own instance.
  pub key: String,
  pub handle: InstanceHandle,
  /// The provider that created the instance and destroys it.
  provider: Arc<dyn Provider>,
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
      provider: native.clone(),
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

/// The instances of every session, by key, and the providers that create
/// them.
pub struct Pool {
  instances: tokio::sync::Mutex<HashMap<String, Arc<Instance>>>,
  native: Arc<Native>,
  remote: Arc<Remote>,
  /// `None` when the Docker client could not be set up (a bad `DOCKER_HOST`).
  docker: Option<Arc<Docker>>,
}

impl Pool {
  pub fn new(native: Arc<Native>, docker: Option<Docker>) -> Self {
    Self {
      instances: tokio::sync::Mutex::new(HashMap::new()),
      native,
      remote: Arc::new(Remote),
      docker: docker.map(Arc::new),
    }
  }

  /// The instance for the spec's key, created through the executor's
  /// provider when no session holds one yet.
  pub async fn acquire(&self, spec: InstanceSpec) -> Result<Arc<Instance>> {
    // TODO(8e): the pool lock is held while the provider creates the
    // instance; slice 8e fixes that with a regression test.
    let mut instances = self.instances.lock().await;
    if let Some(existing) = instances.get(&spec.key) {
      existing.refs.fetch_add(1, Ordering::SeqCst);
      return Ok(existing.clone());
    }
    let provider: Arc<dyn Provider> = match spec.config.runtime {
      Runtime::Native => self.native.clone(),
      Runtime::Remote => self.remote.clone(),
      Runtime::Docker => self.docker.clone().context("Docker is not available on this machine")?,
      // TODO(provider): containerd, firecracker and k8s providers are
      // deferred; see `needy_core_executor::provider`.
      other => bail!("runtime {other:?} is not supported yet"),
    };
    let handle = provider.create(&spec).await?;
    let instance = Arc::new(Instance {
      name: spec.name.clone(),
      key: spec.key.clone(),
      handle,
      provider,
      path_map: spec.config.path_map.clone(),
      slots: spec.config.max_procs.map(|n| Arc::new(Semaphore::new(n as usize))),
      on_full: spec.config.on_full,
      refs: AtomicUsize::new(1),
    });
    instances.insert(spec.key, instance.clone());
    Ok(instance)
  }

  /// Gives back one hold on `instance`; the last one destroys it.
  pub async fn release(&self, instance: &Arc<Instance>) {
    if instance.refs.fetch_sub(1, Ordering::SeqCst) > 1 {
      return;
    }
    let mut instances = self.instances.lock().await;
    instances.retain(|_, v| !Arc::ptr_eq(v, instance));
    if let Err(e) = instance.provider.destroy(&instance.handle).await {
      tracing::warn!("executor {}: cannot destroy {}: {e}", instance.name, instance.handle.id);
    }
  }

  /// Every pooled instance.
  pub async fn instances(&self) -> Vec<Arc<Instance>> {
    self.instances.lock().await.values().cloned().collect()
  }

  /// The one pooled instance of the executor `name`, for a command outside
  /// any session. Instances of the same executor under different keys make
  /// the name ambiguous.
  pub async fn named(&self, name: &str) -> Result<Arc<Instance>, ControlError> {
    let instances = self.instances.lock().await;
    let mut matches: Vec<_> = instances.values().filter(|i| i.name == name).collect();
    match matches.len() {
      0 => Err(ControlError::NotFound("no such executor".into())),
      1 => Ok(matches[0].clone()),
      n => {
        matches.sort_by(|a, b| a.key.cmp(&b.key));
        let keys: Vec<&str> = matches.iter().map(|i| i.key.as_str()).collect();
        Err(ControlError::Invalid(format!("executor {name} is ambiguous: {n} instances ({}); name the session instead", keys.join(", "))))
      }
    }
  }
}

#[cfg(test)]
#[path = "pool_test.rs"]
mod tests;
