//! Synchronization helpers.

use std::sync::{Mutex, MutexGuard};

/// Acquires `mutex`, recovering the inner guard if another thread panicked
/// while holding it.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
  mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
  use std::sync::{Arc, Mutex};

  use super::lock;

  #[test]
  fn lock_acquires_normal_and_poisoned_mutexes() {
    let mutex = Arc::new(Mutex::new(1u32));
    *lock(&mutex) = 2;
    assert_eq!(*lock(&mutex), 2);

    let clone = Arc::clone(&mutex);
    let _ = std::thread::spawn(move || {
      let mut guard = clone.lock().unwrap();
      *guard = 3;
      panic!("intentional poison");
    })
    .join();

    assert!(mutex.is_poisoned());
    assert_eq!(*lock(&mutex), 3);
  }
}
