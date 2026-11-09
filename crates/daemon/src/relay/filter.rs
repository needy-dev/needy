//! The stream filters (`exec.output` interceptors) the relay installs on a
//! session's exec streams.

use std::sync::Arc;

use needy_config::profile::FaultKind;
use needy_core::chaos::Injector;
use needy_core::chaos::sftp::SftpTap;
use needy_core::intercept::{Filtered, StreamFilter};
use needy_core::types::Origin;

use crate::session::Session;

/// The filter for a stream of `session` that runs at `cwd`: the SFTP fault
/// tap on an actor's workspace mount when the profile has `file.open`
/// faults, so a fault can answer `open` itself.
pub fn install(session: &Session, origin: Origin, cwd: &str) -> Option<Arc<dyn StreamFilter>> {
  if origin != Origin::ActorMount || !session.injector.has(FaultKind::FileOpen) {
    return None;
  }
  Some(Arc::new(FileFaults {
    tap: SftpTap::new(cwd),
    injector: session.injector.clone(),
  }))
}

/// `file.open` faults on one SFTP stream, rooted at the mount's source.
struct FileFaults {
  tap: SftpTap,
  injector: Arc<Injector>,
}

impl StreamFilter for FileFaults {
  fn inbound(&self, bytes: &[u8]) -> Filtered {
    let tapped = self.tap.client_bytes(bytes, &self.injector);
    Filtered {
      forward: tapped.forward,
      replies: tapped.replies.into(),
      fired: tapped.injected,
    }
  }

  fn outbound(&self, bytes: &[u8]) -> Vec<u8> {
    self.tap.server_bytes(bytes)
  }
}
