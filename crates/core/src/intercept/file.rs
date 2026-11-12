//! The file domain: `file.open`, whether a file an actor opens on a
//! workspace mount opens. The mount's SFTP stream is the capture, the same
//! for every actor driver; the daemon's relay installs this domain's chain
//! on that stream as a filter on its bytes. The hook point was `exec.output`
//! before the interception domains, after the stream it filters rather than
//! the domain it decides.

use std::sync::Arc;
use std::time::Duration;

use crate::chaos::Injector;
use crate::chaos::sftp::FileFaults;
use crate::types::Fired;

/// A `file.open` interceptor: a filter on one exec stream's bytes, in both
/// directions. It may pass bytes on, hold them until a whole message has
/// arrived or until a fault's delay is over, or answer the caller itself.
pub trait StreamFilter: Send + Sync {
  /// Bytes the caller sends the process.
  fn inbound(&self, bytes: &[u8]) -> Filtered;
  /// Bytes the process sends the caller: what goes on now, and what goes
  /// on later.
  fn outbound(&self, bytes: &[u8]) -> Outbound;
}

/// Bytes for the caller that wait first. The filter frames them whole, so
/// they may be sent between any two other messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Later {
  pub after: Duration,
  pub bytes: Vec<u8>,
}

/// What a [`StreamFilter`] makes of the caller's bytes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Filtered {
  /// Bytes for the process.
  pub forward: Vec<u8>,
  /// Answers for the caller, in order, which the process never sees.
  pub replies: Vec<Later>,
  /// Faults the filter injected.
  pub fired: Vec<Fired>,
}

/// What a [`StreamFilter`] makes of the process's bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outbound {
  /// Bytes for the caller now.
  pub now: Vec<u8>,
  /// Bytes for the caller after a fault's delay.
  pub later: Vec<Later>,
}

/// The `file.open` chain of the SFTP stream of a workspace mount of the
/// executor's `root` at `target` in the actor: chaos's file faults.
// NOTICE: installed whether or not the run has file faults now, because
// `needy chaos add` may add one while the mount is open; framing a stream
// that no fault matches only buffers partial packets. One filter is the
// whole chain today; a second `file.open` interceptor would make this a
// filter that runs several in order.
pub fn chain(injector: &Arc<Injector>, root: &str, target: &str) -> Arc<dyn StreamFilter> {
  Arc::new(FileFaults::new(root, target, injector.clone()))
}
