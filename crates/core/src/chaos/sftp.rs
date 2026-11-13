//! SFTP packet framing for file faults. File faults act on SFTP traffic
//! between an actor's sshfs mount and `sftp-server` on the executor: a
//! matching `open` that fails never reaches the executor and gets an error
//! reply instead, and the reply to one that is delayed is held. Pure bytes;
//! the relay feeds both directions through [`FileFaults`], the `file.open`
//! interceptor, and sends what it holds when its time comes.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::{Injector, Subject};
use crate::intercept::file::{Filtered, Later, Outbound, StreamFilter};
use crate::types::Fired;
use needy_common::sftp::{SSH_FXP_OPEN, read_u32, status_reply, take_packets};

/// SFTP status code for an errno. sshfs maps unknown codes to EIO.
// Source: https://github.com/libfuse/sshfs/blob/sshfs-3.7.6/sshfs.c#L1766-L1779
// (`sftp_error_to_errno`: 2 ENOENT, 3 EACCES, 4 EPERM, 8 EOPNOTSUPP, any
// other EIO), the inverse of this table; codes from
// https://github.com/openssh/openssh-portable/blob/V_10_6_P2/sftp.h#L89-L95.
// `needy_config::profile::FILE_ERRNOS` refuses every other errno when the
// profile loads.
// NOTICE: sshfs turns SSH_FX_FAILURE on a `rmdir` into ENOTEMPTY
// (sshfs.c#L2027-L2031); only `open` is faulted here. 255 is no SFTP code,
// which sshfs reads as EIO.
fn sftp_code(errno: &str) -> u32 {
  match errno {
    "ENOENT" => 2,
    "EACCES" => 3,
    "EPERM" => 4,
    "EOPNOTSUPP" => 8,
    _ => 255,
  }
}

/// One mount's SFTP stream, framed in both directions.
pub struct SftpTap {
  to_server: Mutex<Vec<u8>>,
  to_client: Mutex<Vec<u8>>,
  /// The mount's source on the executor, which the paths sshfs sends start
  /// with.
  root: String,
  /// Where the mount is in the actor: faults match the path there.
  target: String,
  /// The requests whose reply is held, by request ID, until this time.
  held: Mutex<HashMap<u32, Instant>>,
}

pub struct Tapped {
  /// Bytes to forward to `sftp-server`.
  pub forward: Vec<u8>,
  /// Replies to send straight back to sshfs, each after its delay.
  pub replies: VecDeque<Later>,
  /// The faults injected into `open` requests.
  pub injected: Vec<Fired>,
}

impl SftpTap {
  /// The tap of a mount of the executor's `root` at `target` in the actor.
  pub fn new(root: &str, target: &str) -> Self {
    Self {
      to_server: Mutex::new(Vec::new()),
      to_client: Mutex::new(Vec::new()),
      root: root.trim_end_matches('/').to_string(),
      target: target.trim_end_matches('/').to_string(),
      held: Mutex::new(HashMap::new()),
    }
  }

  /// The path in the actor of a path sshfs sends.
  fn actor_path(&self, path: &str) -> String {
    let relative = path.strip_prefix(&self.root).unwrap_or(path).trim_start_matches('/');
    if relative.is_empty() {
      self.target.clone()
    } else {
      format!("{}/{relative}", self.target)
    }
  }

  /// Feeds bytes from sshfs; complete packets are forwarded or answered.
  pub fn client_bytes(&self, bytes: &[u8], injector: &Injector) -> Tapped {
    let mut buf = self.to_server.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    buf.extend_from_slice(bytes);
    let mut tapped = Tapped {
      forward: Vec::new(),
      replies: VecDeque::new(),
      injected: Vec::new(),
    };
    for packet in take_packets(&mut buf) {
      if packet.get(4) == Some(&SSH_FXP_OPEN)
        && let (Some(id), Some(name_len)) = (read_u32(&packet[5..]), read_u32(&packet[9..]))
        && let Some(name) = packet.get(13..13 + name_len as usize)
      {
        let path = self.actor_path(&String::from_utf8_lossy(name));
        if let Some(fired) = injector.check(Subject::FileOpen(&path)) {
          let delay = fired.effect.delay.unwrap_or(Duration::ZERO);
          match &fired.effect.errno {
            Some(errno) => {
              tapped.replies.push_back(Later {
                after: delay,
                bytes: status_reply(id, sftp_code(errno), &format!("needy chaos: {errno}")),
              });
              tapped.injected.push(fired);
              continue;
            }
            // A delay alone holds the open's reply, not the stream: the
            // open goes on, and its reply waits.
            None => {
              self.held.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).insert(id, Instant::now() + delay);
              tapped.injected.push(fired);
            }
          }
        }
      }
      tapped.forward.extend(packet);
    }
    tapped
  }

  /// Feeds bytes from `sftp-server`; returns only complete packets, so
  /// injected replies never land in the middle of one, and holds the reply
  /// to a delayed open until its time.
  pub fn server_bytes(&self, bytes: &[u8]) -> Outbound {
    let mut buf = self.to_client.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    buf.extend_from_slice(bytes);
    let mut out = Outbound::default();
    let mut held = self.held.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    for packet in take_packets(&mut buf) {
      // Every reply carries its request's ID after its type.
      match read_u32(packet.get(5..).unwrap_or_default()).and_then(|id| held.remove(&id)) {
        Some(until) => out.later.push(Later {
          after: until.saturating_duration_since(Instant::now()),
          bytes: packet,
        }),
        None => out.now.extend(packet),
      }
    }
    out
  }
}

/// File faults on one SFTP stream: the `file.open` interceptor, which
/// [`crate::intercept::file::chain`] installs.
pub struct FileFaults {
  tap: SftpTap,
  injector: Arc<Injector>,
}

impl FileFaults {
  /// File faults on a mount of the executor's `root` at `target` in the
  /// actor.
  pub fn new(root: &str, target: &str, injector: Arc<Injector>) -> Self {
    Self {
      tap: SftpTap::new(root, target),
      injector,
    }
  }
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

  fn outbound(&self, bytes: &[u8]) -> Outbound {
    self.tap.server_bytes(bytes)
  }
}

#[cfg(test)]
#[path = "sftp_test.rs"]
mod tests;
