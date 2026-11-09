//! Where events go. The dispatch worker owns its sinks and calls them one
//! event at a time, in order.

#[cfg(feature = "jsonl")]
pub mod jsonl;
#[cfg(feature = "memory")]
pub mod memory;
#[cfg(feature = "sqlite")]
pub mod sqlite;

use crate::{Error, Event};

#[async_trait::async_trait]
pub trait Sink: Send {
  async fn write(&mut self, event: &Event) -> Result<(), Error>;

  /// Makes what was written durable or visible to readers.
  async fn flush(&mut self) -> Result<(), Error> {
    Ok(())
  }
}
