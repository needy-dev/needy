//! Admission by the resolved executor pool key. Only actual process exit
//! releases capacity; streams and control instances do not own these permits.

use super::Error;
use needy_config::profile::OnFull;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(crate) struct Capacity {
  pub key: String,
  pub max_procs: u32,
  pub on_full: OnFull,
}

#[derive(Default)]
pub(super) struct Limits(Mutex<HashMap<String, Entry>>);

struct Entry {
  max_procs: u32,
  on_full: OnFull,
  semaphore: Weak<Semaphore>,
}

impl Limits {
  pub async fn reserve(&self, policy: Capacity) -> Result<OwnedSemaphorePermit, Error> {
    if policy.key.is_empty() || policy.max_procs as usize > Semaphore::MAX_PERMITS {
      return Err(Error::InvalidCapacity);
    }
    let semaphore = {
      let mut entries = self.0.lock().unwrap_or_else(|e| e.into_inner());
      // Permits and pending admissions keep their semaphore alive. Reclaim
      // completed keys at the next admission instead of retaining every run.
      entries.retain(|_, entry| entry.semaphore.strong_count() > 0);
      match entries.get(&policy.key).and_then(|entry| entry.semaphore.upgrade().map(|semaphore| (entry, semaphore))) {
        Some((entry, _)) if entry.max_procs != policy.max_procs || entry.on_full != policy.on_full => return Err(Error::CapacityChanged),
        Some((_, semaphore)) => semaphore,
        None => {
          let semaphore = Arc::new(Semaphore::new(policy.max_procs as usize));
          entries.insert(
            policy.key,
            Entry {
              max_procs: policy.max_procs,
              on_full: policy.on_full,
              semaphore: Arc::downgrade(&semaphore),
            },
          );
          semaphore
        }
      }
    };
    match policy.on_full {
      OnFull::Reject => semaphore.try_acquire_owned().map_err(|_| Error::Full),
      OnFull::Wait => semaphore.acquire_owned().await.map_err(|_| Error::ShuttingDown),
    }
  }
}
