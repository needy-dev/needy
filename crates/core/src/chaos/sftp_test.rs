use needy_config::profile::{Chaos, FaultKind};

use super::*;
use crate::chaos::Injector;

fn chaos(yaml: &str) -> Chaos {
  serde_yaml::from_str(yaml).unwrap()
}

fn open_packet(id: u32, path: &str) -> Vec<u8> {
  let mut body = vec![SSH_FXP_OPEN];
  body.extend(id.to_be_bytes());
  body.extend((path.len() as u32).to_be_bytes());
  body.extend(path.as_bytes());
  body.extend(1u32.to_be_bytes()); // pflags
  body.extend(0u32.to_be_bytes()); // attrs flags
  let mut p = (body.len() as u32).to_be_bytes().to_vec();
  p.extend(body);
  p
}

#[test]
fn sftp_open_is_answered_with_an_error() {
  let i = Injector::new(&chaos("faults: [{on: file.open, match: '**/*.json', errno: EIO}]"));
  let tap = SftpTap::new("/home/agent/workspace/");
  let mut stream = open_packet(7, "/home/agent/workspace/src/a.txt");
  stream.extend(open_packet(8, "/home/agent/workspace/src/package.json"));
  // Split mid-packet to exercise framing.
  let (a, b) = stream.split_at(10);
  let first = tap.client_bytes(a, &i);
  assert!(first.forward.is_empty() && first.replies.is_empty());
  let second = tap.client_bytes(b, &i);
  assert_eq!(second.forward, open_packet(7, "/home/agent/workspace/src/a.txt"));
  assert_eq!(second.replies.len(), 1);
  let reply = &second.replies[0];
  assert_eq!(reply[4], SSH_FXP_STATUS);
  assert_eq!(read_u32(&reply[5..]), Some(8));
  assert_eq!(read_u32(&reply[9..]), Some(255));
  assert_eq!(second.injected.len(), 1);
  assert_eq!(second.injected[0].kind, FaultKind::FileOpen);
  assert_eq!(second.injected[0].target, "src/package.json");
  assert_eq!(second.injected[0].effect.errno.as_deref(), Some("EIO"));
}

#[test]
fn server_output_is_released_whole() {
  let tap = SftpTap::new("/w");
  let packet = status_reply(1, 0, "ok");
  assert!(tap.server_bytes(&packet[..5]).is_empty());
  assert_eq!(tap.server_bytes(&packet[5..]), packet);
}
