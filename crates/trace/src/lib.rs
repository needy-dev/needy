//! Needy's event pipeline. Producers hand [`Dispatch`] an event kind and its
//! JSON payload; Dispatch stamps the [`Event`] envelope and passes it to the
//! run's sinks without ever blocking the producer.
//!
//! The crate does not know event types: payloads are JSON built by
//! `needy_core::event`, which also reads them back. It reaches the filesystem
//! and SQLite only through the sinks, each behind a feature.

pub mod dispatch;
pub mod event;
pub mod sink;
#[cfg(feature = "timing")]
pub mod timing;

pub use dispatch::{Dispatch, Options};
pub use event::Event;
pub use sink::Sink;

/// A sink or reader failure.
#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("{path}: {source}")]
  Io {
    path: std::path::PathBuf,
    #[source]
    source: std::io::Error,
  },
  #[error("event is not JSON: {0}")]
  Json(#[from] serde_json::Error),
  #[cfg(feature = "sqlite")]
  #[error("trace database: {0}")]
  Database(#[from] sea_orm::DbErr),
  #[cfg(feature = "sqlite")]
  #[error("trace database {0} has another schema; needy does not migrate trace databases from an earlier schema, so move it away")]
  Schema(std::path::PathBuf),
  #[error("no trace database at {0}")]
  NoDatabase(std::path::PathBuf),
  #[error("no run matches {0}")]
  NoRun(String),
  #[error("run prefix {prefix} is ambiguous; give a full ID (candidates: {candidates:?})")]
  AmbiguousRun {
    prefix: String,
    candidates: Vec<String>,
  },
}
