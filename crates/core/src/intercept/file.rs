//! The file domain: `file.open`, whether a file an actor opens on a
//! workspace mount opens. The mount's SFTP stream is the capture, the same
//! for every actor driver; the daemon's relay installs this domain's chain
//! on that stream as a filter on its bytes. The hook point was `exec.output`
//! before the interception domains, after the stream it filters rather than
//! the domain it decides.

use std::sync::Arc;

use needy_config::profile::FaultKind;

use crate::chaos::Injector;
use crate::chaos::sftp::FileFaults;
use crate::types::Fired;

/// A `file.open` interceptor: a filter on one exec stream's bytes, in both
/// directions. It may pass bytes on, hold them until a whole message has
/// arrived, or answer the caller itself.
pub trait StreamFilter: Send + Sync {
  /// Bytes the caller sends the process.
  fn inbound(&self, bytes: &[u8]) -> Filtered;
  /// Bytes the process sends the caller; what it returns goes on.
  fn outbound(&self, bytes: &[u8]) -> Vec<u8>;
}

/// What a [`StreamFilter`] makes of the caller's bytes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Filtered {
  /// Bytes for the process.
  pub forward: Vec<u8>,
  /// Answers for the caller, in order, which the process never sees.
  pub replies: Vec<Vec<u8>>,
  /// Faults the filter injected.
  pub fired: Vec<Fired>,
}

/// The `file.open` chain of one workspace mount's SFTP stream, rooted at the
/// mount's source `root`: chaos's file faults when the run has `file.open`
/// faults, or nothing, and the stream is not filtered.
// NOTICE: one filter is the whole chain today. A second `file.open`
// interceptor would make this a filter that runs several in order.
pub fn chain(injector: &Arc<Injector>, root: &str) -> Option<Arc<dyn StreamFilter>> {
  if !injector.has(FaultKind::FileOpen) {
    return None;
  }
  Some(Arc::new(FileFaults::new(root, injector.clone())))
}
