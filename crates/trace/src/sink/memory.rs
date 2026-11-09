//! A sink that keeps events in memory, for tests.

use std::sync::{Arc, Mutex, PoisonError};

use crate::{Error, Event, Sink};

/// Clones share one list.
#[derive(Clone, Default)]
pub struct Memory {
  events: Arc<Mutex<Vec<Event>>>,
}

impl Memory {
  pub fn new() -> Self {
    Self::default()
  }

  /// The events written so far, in order.
  pub fn events(&self) -> Vec<Event> {
    self.events.lock().unwrap_or_else(PoisonError::into_inner).clone()
  }
}

#[async_trait::async_trait]
impl Sink for Memory {
  async fn write(&mut self, event: &Event) -> Result<(), Error> {
    self.events.lock().unwrap_or_else(PoisonError::into_inner).push(event.clone());
    Ok(())
  }
}
