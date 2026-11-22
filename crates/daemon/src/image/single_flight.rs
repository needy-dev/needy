//! Async single-flight deduplication table for concurrent image operations.

use std::collections::HashMap;
use std::future::Future;
use std::hash::Hash;
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow, bail};
use needy_common::sync::lock;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

type FlightOutcome<V> = std::result::Result<V, String>;
type FlightMap<K, V> = HashMap<K, watch::Receiver<Option<FlightOutcome<V>>>>;

/// Generic async single-flight deduplication table: concurrent callers for the
/// same key share one leader execution and receive a clone of its result.
pub(super) struct SingleFlight<K, V> {
  in_flight: Arc<Mutex<FlightMap<K, V>>>,
}

impl<K, V> Default for SingleFlight<K, V> {
  fn default() -> Self {
    Self {
      in_flight: Arc::default(),
    }
  }
}

enum Role<V> {
  Leader(watch::Sender<Option<FlightOutcome<V>>>),
  Waiter(watch::Receiver<Option<FlightOutcome<V>>>),
}

struct Guard<K: Eq + Hash, V> {
  in_flight: Arc<Mutex<FlightMap<K, V>>>,
  key: K,
}

impl<K: Eq + Hash, V> Drop for Guard<K, V> {
  fn drop(&mut self) {
    lock(&self.in_flight).remove(&self.key);
  }
}

impl<K, V> SingleFlight<K, V>
where
  K: Eq + Hash + Clone,
  V: Clone,
{
  fn enter(&self, key: &K) -> Role<V> {
    let mut map = lock(&self.in_flight);
    if let Some(rx) = map.get(key) {
      Role::Waiter(rx.clone())
    } else {
      let (tx, rx) = watch::channel(None);
      map.insert(key.clone(), rx);
      Role::Leader(tx)
    }
  }

  /// Runs `work` once per concurrent `key`; waiters receive a clone of the
  /// leader's result.
  pub(super) async fn run<F, Fut>(&self, key: K, work: F) -> Result<V>
  where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<V>>,
  {
    match self.enter(&key) {
      Role::Waiter(mut rx) => {
        let result = rx
          .wait_for(Option::is_some)
          .await
          .map_err(|_| anyhow!("in-flight image operation ended without a result"))?
          .clone()
          .expect("wait_for guarantees Some");
        result.map_err(|msg| anyhow!(msg))
      }
      Role::Leader(tx) => {
        let _guard = Guard {
          in_flight: self.in_flight.clone(),
          key,
        };
        match work().await {
          Ok(val) => {
            let _ = tx.send(Some(Ok(val.clone())));
            Ok(val)
          }
          Err(err) => {
            let _ = tx.send(Some(Err(format!("{err:#}"))));
            Err(err)
          }
        }
      }
    }
  }

  /// Like [`Self::run`], except waiters also stop if `cancel` is triggered,
  /// returning `cancel_message`.
  pub(super) async fn run_cancellable<F, Fut>(&self, key: K, cancel: &CancellationToken, cancel_message: &str, work: F) -> Result<V>
  where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<V>>,
  {
    match self.enter(&key) {
      Role::Waiter(mut rx) => {
        let result = tokio::select! {
          () = cancel.cancelled() => bail!("{cancel_message}"),
          changed = rx.wait_for(Option::is_some) => match changed {
            Ok(guard) => guard.clone().expect("wait_for guarantees Some"),
            Err(_) => bail!("the in-flight image operation ended without a result"),
          }
        };
        result.map_err(|msg| anyhow!(msg))
      }
      Role::Leader(tx) => {
        let _guard = Guard {
          in_flight: self.in_flight.clone(),
          key,
        };
        match work().await {
          Ok(val) => {
            let _ = tx.send(Some(Ok(val.clone())));
            Ok(val)
          }
          Err(err) => {
            let _ = tx.send(Some(Err(format!("{err:#}"))));
            Err(err)
          }
        }
      }
    }
  }
}
