//! The file domain's adapter: the stream filters (`file.open` interceptors)
//! the relay installs on a run's exec streams.

use std::sync::Arc;

use needy_core::intercept::file::{self, StreamFilter};
use needy_core::types::Origin;

use crate::run::Run;

/// The filter for a stream of `run` that runs at `cwd`: the `file.open`
/// chain on an actor's workspace mount (its SFTP stream, rooted at the
/// mount's source), so a fault can answer `open` itself. Other streams are
/// not filtered.
pub fn install(run: &Run, origin: Origin, cwd: &str) -> Option<Arc<dyn StreamFilter>> {
  if origin != Origin::ActorMount {
    return None;
  }
  file::chain(&run.injector, cwd)
}
