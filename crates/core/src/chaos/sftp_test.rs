use std::time::Duration;

use needy_config::profile::{Chaos, FaultDomain};

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

/// `SSH_FXP_HANDLE` (102) for request `id`.
fn handle_reply(id: u32) -> Vec<u8> {
  let mut body = vec![102];
  body.extend(id.to_be_bytes());
  body.extend(4u32.to_be_bytes());
  body.extend(b"h001");
  let mut p = (body.len() as u32).to_be_bytes().to_vec();
  p.extend(body);
  p
}

#[test]
fn sftp_open_is_answered_with_an_error() {
  let i = Injector::new(&chaos("faults: { json: {file: {open: '**/*.json'}, errno: EIO} }"));
  // The executor's directory is mounted at the actor's workspace.
  let tap = SftpTap::new("/srv/ws/", "/home/agent/workspace");
  let mut stream = open_packet(7, "/srv/ws/src/a.txt");
  stream.extend(open_packet(8, "/srv/ws/src/package.json"));
  // Split mid-packet to exercise framing.
  let (a, b) = stream.split_at(10);
  let first = tap.client_bytes(a, &i);
  assert!(first.forward.is_empty() && first.replies.is_empty());
  let second = tap.client_bytes(b, &i);
  assert_eq!(second.forward, open_packet(7, "/srv/ws/src/a.txt"));
  assert_eq!(second.replies.len(), 1);
  let reply = &second.replies[0];
  assert_eq!(reply.after, Duration::ZERO);
  assert_eq!(reply.bytes[4], SSH_FXP_STATUS);
  assert_eq!(read_u32(&reply.bytes[5..]), Some(8));
  assert_eq!(read_u32(&reply.bytes[9..]), Some(255));
  assert_eq!(second.injected.len(), 1);
  assert_eq!(second.injected[0].domain, FaultDomain::File);
  assert_eq!(second.injected[0].name, "json");
  // The path as the actor sees it.
  assert_eq!(second.injected[0].target, "/home/agent/workspace/src/package.json");
  assert_eq!(second.injected[0].effect.errno.as_deref(), Some("EIO"));
}

#[test]
fn each_deliverable_errno_has_the_code_sshfs_turns_back_into_it() {
  assert_eq!(
    needy_config::profile::FILE_ERRNOS.map(sftp_code),
    [2, 3, 4, 8, 255],
    "ENOENT, EACCES, EPERM, EOPNOTSUPP, and EIO from a code sshfs does not know"
  );
}

#[test]
fn a_delay_and_an_errno_answer_after_the_delay() {
  let i = Injector::new(&chaos("faults: { slow-fail: {file: {open: '**'}, errno: ENOENT, delay: 2s} }"));
  let tap = SftpTap::new("/srv/ws", "/w");
  let tapped = tap.client_bytes(&open_packet(3, "/srv/ws/x"), &i);
  assert!(tapped.forward.is_empty());
  assert_eq!(tapped.replies[0].after, Duration::from_secs(2));
  assert_eq!(read_u32(&tapped.replies[0].bytes[9..]), Some(2));
}

#[test]
fn a_delay_alone_holds_the_open_s_reply_and_nothing_else() {
  let i = Injector::new(&chaos("faults: { slow: {file: {open: '**/slow'}, delay: 1s} }"));
  let tap = SftpTap::new("/srv/ws", "/w");
  // The open goes on to the server.
  let tapped = tap.client_bytes(&open_packet(5, "/srv/ws/slow"), &i);
  assert_eq!(tapped.forward, open_packet(5, "/srv/ws/slow"));
  assert_eq!(tapped.injected.len(), 1);
  // Its reply waits; another request's does not.
  let mut replies = handle_reply(6);
  replies.extend(handle_reply(5));
  let out = tap.server_bytes(&replies);
  assert_eq!(out.now, handle_reply(6));
  assert_eq!(out.later.len(), 1);
  assert_eq!(out.later[0].bytes, handle_reply(5));
  assert!(out.later[0].after > Duration::from_millis(500) && out.later[0].after <= Duration::from_secs(1));
}

#[test]
fn server_output_is_released_whole() {
  let tap = SftpTap::new("/w", "/w");
  let packet = status_reply(1, 0, "ok");
  assert!(tap.server_bytes(&packet[..5]).now.is_empty());
  assert_eq!(tap.server_bytes(&packet[5..]).now, packet);
}
