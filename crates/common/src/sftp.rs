//! SFTP packet framing, as `sftp-server` and sshfs speak it (protocol version
//! 3): split a byte stream into packets, read a packet's type and request ID,
//! and build an `SSH_FXP_STATUS` reply. Pure bytes; the file faults of chaos
//! and the workspace mount's stream both frame with it.

// Source: https://github.com/openssh/openssh-portable/blob/V_10_6_P2/sftp.h#L35-L36
// (`SSH2_FXP_INIT 1`, `SSH2_FXP_VERSION 2`, `SSH2_FXP_OPEN 3`) and #L58
// (`SSH2_FXP_STATUS 101`), protocol version 3 (#L32), which `sftp-server`
// speaks.
// Reference: https://datatracker.ietf.org/doc/html/draft-ietf-secsh-filexfer-02#section-3
// (packet `uint32 length, byte type`, then `uint32 id` for every type but
// INIT and VERSION, section 6.3) and #section-7 (SSH_FXP_STATUS: id, code,
// message, language tag).
pub const SSH_FXP_INIT: u8 = 1;
pub const SSH_FXP_VERSION: u8 = 2;
pub const SSH_FXP_OPEN: u8 = 3;
pub const SSH_FXP_STATUS: u8 = 101;

// Source: https://github.com/openssh/openssh-portable/blob/V_10_6_P2/sftp.h#L89-L97
// (`SSH2_FX_FAILURE 4` .. `SSH2_FX_CONNECTION_LOST 7`). sshfs reads both as
// EIO (`sftp_error_to_errno`,
// https://github.com/libfuse/sshfs/blob/sshfs-3.7.6/sshfs.c#L1766-L1779).
pub const SSH_FX_CONNECTION_LOST: u32 = 7;

/// A big-endian `uint32` at the start of `b`.
pub fn read_u32(b: &[u8]) -> Option<u32> {
  Some(u32::from_be_bytes(b.get(..4)?.try_into().ok()?))
}

/// Splits complete SFTP packets off the front of `buf`.
pub fn take_packets(buf: &mut Vec<u8>) -> Vec<Vec<u8>> {
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

/// A whole packet's type.
pub fn packet_type(packet: &[u8]) -> Option<u8> {
  packet.get(4).copied()
}

/// A whole packet's request ID: none for INIT and VERSION, which carry a
/// version there instead.
pub fn request_id(packet: &[u8]) -> Option<u32> {
  match packet_type(packet)? {
    SSH_FXP_INIT | SSH_FXP_VERSION => None,
    _ => read_u32(packet.get(5..)?),
  }
}

/// Builds `SSH_FXP_STATUS` for request `id`.
pub fn status_reply(id: u32, code: u32, message: &str) -> Vec<u8> {
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn packets_split_at_their_length_and_keep_a_partial_one() {
    let mut buf = status_reply(1, 0, "ok");
    buf.extend(status_reply(2, 4, "no"));
    let partial = status_reply(3, 0, "later");
    buf.extend(&partial[..6]);

    let packets = take_packets(&mut buf);

    assert_eq!(packets.len(), 2);
    assert_eq!(packets.iter().map(|p| request_id(p)).collect::<Vec<_>>(), [Some(1), Some(2)]);
    assert_eq!(buf, partial[..6]);
  }

  #[test]
  fn init_and_version_carry_no_request_id() {
    // INIT: length 5, type 1, version 3.
    let init = [0, 0, 0, 5, SSH_FXP_INIT, 0, 0, 0, 3];

    assert_eq!(packet_type(&init), Some(SSH_FXP_INIT));
    assert_eq!(request_id(&init), None);
  }
}
