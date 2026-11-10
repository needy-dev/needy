//! SFTP packet framing for `file.open` faults. File faults act on SFTP
//! traffic between an actor's sshfs mount and `sftp-server` on the executor:
//! a matching `open` never reaches the executor and gets an error reply
//! instead. Pure bytes; the relay feeds both directions through a tap.

use std::collections::VecDeque;
use std::sync::Mutex;

use super::{Injector, Subject};
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
// NOTICE: sshfs turns SSH_FX_FAILURE on a `rmdir` into ENOTEMPTY
// (sshfs.c#L2027-L2031); only `open` is faulted here.
fn sftp_code(errno: &str) -> u32 {
  match errno {
    "ENOENT" => 2,
    "EACCES" => 3,
    "EPERM" => 4,
    "EOPNOTSUPP" | "ENOTSUP" => 8,
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
  /// Paths relative to the mount root are matched against faults.
  root: String,
}

pub struct Tapped {
  /// Bytes to forward to `sftp-server`.
  pub forward: Vec<u8>,
  /// Replies to send straight back to sshfs.
  pub replies: VecDeque<Vec<u8>>,
  /// The faults injected into `open` requests.
  pub injected: Vec<Fired>,
}

impl SftpTap {
  pub fn new(root: &str) -> Self {
    Self {
      to_server: Mutex::new(Vec::new()),
      to_client: Mutex::new(Vec::new()),
      root: root.trim_end_matches('/').to_string(),
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
        let path = String::from_utf8_lossy(name).to_string();
        let relative = path.strip_prefix(&self.root).unwrap_or(&path).trim_start_matches('/').to_string();
        if let Some(mut fired) = injector.check(Subject::FileOpen(&relative)) {
          // NOTICE: a delay-only fault still fails the open; the tap cannot
          // hold one request without holding the stream behind it.
          let errno = fired.effect.errno.get_or_insert_with(|| "EIO".into()).clone();
          tapped.replies.push_back(status_reply(id, sftp_code(&errno), &format!("needy chaos: {errno}")));
          tapped.injected.push(fired);
          continue;
        }
      }
      tapped.forward.extend(packet);
    }
    tapped
  }

  /// Feeds bytes from `sftp-server`; returns only complete packets, so
  /// injected replies never land in the middle of one.
  pub fn server_bytes(&self, bytes: &[u8]) -> Vec<u8> {
    let mut buf = self.to_client.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    buf.extend_from_slice(bytes);
    take_packets(&mut buf).concat()
  }
}

#[cfg(test)]
#[path = "sftp_test.rs"]
mod tests;
