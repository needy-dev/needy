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

// Source: https://github.com/openssh/openssh-portable/blob/V_10_6_P2/sftp.h#L36
// (`SSH2_FXP_OPEN 3`) and #L58 (`SSH2_FXP_STATUS 101`), protocol version 3
// (#L32), which `sftp-server` speaks.
// Reference: https://datatracker.ietf.org/doc/html/draft-ietf-secsh-filexfer-02#section-3
// (packet `uint32 length, byte type`, then `uint32 id`, section 6.3) and
// #section-7 (SSH_FXP_STATUS: id, code, message, language tag).
const SSH_FXP_OPEN: u8 = 3;
const SSH_FXP_STATUS: u8 = 101;

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

fn read_u32(b: &[u8]) -> Option<u32> {
  Some(u32::from_be_bytes(b.get(..4)?.try_into().ok()?))
}

/// Splits complete SFTP packets off the front of `buf`.
fn take_packets(buf: &mut Vec<u8>) -> Vec<Vec<u8>> {
  let mut out = Vec::new();
  while let Some(len) = read_u32(buf) {
    let total = 4 + len as usize;
    if buf.len() < total {
      break;
    }
    out.push(buf.drain(..total).collect());
  }
  out
}

/// Builds `SSH_FXP_STATUS` for request `id`.
fn status_reply(id: u32, code: u32, message: &str) -> Vec<u8> {
  let mut body = vec![SSH_FXP_STATUS];
  body.extend(id.to_be_bytes());
  body.extend(code.to_be_bytes());
  body.extend((message.len() as u32).to_be_bytes());
  body.extend(message.as_bytes());
  body.extend(0u32.to_be_bytes()); // empty language tag
  let mut packet = (body.len() as u32).to_be_bytes().to_vec();
  packet.extend(body);
  packet
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
