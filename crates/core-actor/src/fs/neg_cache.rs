//! Bounded FIFO-expiring negative dentry cache.
//!
//! Caches `ENOENT` lookup results keyed by `(parent_ino, name)` for a short TTL
//! so repeated misses (e.g., PATH/include/module searches) avoid WAN RTTs.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Default maximum number of negative entries held simultaneously.
pub const DEFAULT_CAPACITY: usize = 4096;
/// Default time-to-live for cached `ENOENT` results.
pub const DEFAULT_TTL: Duration = Duration::from_secs(1);

/// Thread-safe bounded FIFO-expiring TTL cache for negative lookups.
pub struct NegativeCache {
  inner: Mutex<Inner>,
}

struct Inner {
  entries: HashMap<(u64, String), Instant>,
  order: VecDeque<((u64, String), Instant)>,
  capacity: usize,
  ttl: Duration,
}

impl Default for NegativeCache {
  fn default() -> Self {
    Self::new(DEFAULT_CAPACITY, DEFAULT_TTL)
  }
}

impl NegativeCache {
  /// Creates a new negative cache with the given capacity and TTL.
  pub fn new(capacity: usize, ttl: Duration) -> Self {
    Self {
      inner: Mutex::new(Inner {
        entries: HashMap::new(),
        order: VecDeque::new(),
        capacity,
        ttl,
      }),
    }
  }

  /// Returns `true` if `(parent, name)` has a live (unexpired) negative entry.
  pub fn contains(&self, parent: u64, name: &str) -> bool {
    let mut inner = self.inner.lock().expect("neg_cache lock poisoned");
    let now = Instant::now();
    let key = (parent, name.to_string());
    match inner.entries.get(&key) {
      Some(&expires_at) if now < expires_at => true,
      Some(_) => {
        inner.entries.remove(&key);
        false
      }
      None => false,
    }
  }

  /// Inserts or refreshes a negative entry for `(parent, name)`.
  pub fn insert(&self, parent: u64, name: impl Into<String>) {
    let mut inner = self.inner.lock().expect("neg_cache lock poisoned");
    if inner.capacity == 0 || inner.ttl.is_zero() {
      return;
    }

    let now = Instant::now();
    inner.prune_expired(now);

    let key = (parent, name.into());
    let expires_at = now + inner.ttl;

    if !inner.entries.contains_key(&key) {
      while inner.entries.len() >= inner.capacity {
        let Some((old_key, old_exp)) = inner.order.pop_front() else {
          break;
        };
        if inner.entries.get(&old_key) == Some(&old_exp) {
          inner.entries.remove(&old_key);
        }
      }
    }

    inner.entries.insert(key.clone(), expires_at);
    inner.order.push_back((key, expires_at));
  }

  /// Removes a single entry (e.g. on `create`, `mkdir`, `symlink`, `link`, `rename`).
  pub fn invalidate(&self, parent: u64, name: &str) {
    let mut inner = self.inner.lock().expect("neg_cache lock poisoned");
    inner.entries.remove(&(parent, name.to_string()));
  }

  /// Removes all cached negative entries under `parent`.
  pub fn invalidate_parent(&self, parent: u64) {
    let mut inner = self.inner.lock().expect("neg_cache lock poisoned");
    inner.entries.retain(|(p, _), _| *p != parent);
  }

  /// Removes all entries from the cache.
  pub fn clear(&self) {
    let mut inner = self.inner.lock().expect("neg_cache lock poisoned");
    inner.entries.clear();
    inner.order.clear();
  }

  /// Number of entries currently stored (including not-yet-pruned expired entries).
  pub fn len(&self) -> usize {
    let inner = self.inner.lock().expect("neg_cache lock poisoned");
    inner.entries.len()
  }

  /// Returns `true` if no entries are currently stored.
  pub fn is_empty(&self) -> bool {
    self.len() == 0
  }
}

impl Inner {
  fn prune_expired(&mut self, now: Instant) {
    while let Some((_, exp)) = self.order.front() {
      if now < *exp {
        break;
      }
      if let Some((old_key, old_exp)) = self.order.pop_front()
        && self.entries.get(&old_key) == Some(&old_exp)
      {
        self.entries.remove(&old_key);
      }
    }
  }
}
