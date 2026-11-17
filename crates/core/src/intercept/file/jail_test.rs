use needy_common::sftp::{read_u32, take_packets};

use super::*;
use crate::intercept::file::StreamFilter;

const ROOT: &str = "/home/user/.local/share/needy/sync/runs/r1/pi/copy";

fn string(out: &mut Vec<u8>, s: &str) {
  out.extend((s.len() as u32).to_be_bytes());
  out.extend(s.as_bytes());
}

/// A request packet of `kind` with request ID `id` and `body` after the ID.
fn packet(kind: u8, id: u32, body: &[u8]) -> Vec<u8> {
  let mut inner = vec![kind];
  inner.extend(id.to_be_bytes());
  inner.extend(body);
  let mut out = (inner.len() as u32).to_be_bytes().to_vec();
  out.extend(inner);
  out
}

fn paths(kind: u8, id: u32, paths: &[&str]) -> Vec<u8> {
  let mut body = Vec::new();
  for p in paths {
    string(&mut body, p);
  }
  // Attributes or flags after the paths: a zero `uint32` stands for them.
  body.extend(0u32.to_be_bytes());
  packet(kind, id, &body)
}

fn extended(id: u32, name: &str, args: &[&str]) -> Vec<u8> {
  let mut body = Vec::new();
  string(&mut body, name);
  for a in args {
    string(&mut body, a);
  }
  packet(200, id, &body)
}

/// The status codes of the replies the jail sent back, by request ID.
fn refused(filtered: &crate::intercept::file::Filtered) -> Vec<(u32, u32)> {
  let mut bytes: Vec<u8> = filtered.replies.iter().flat_map(|r| r.bytes.clone()).collect();
  take_packets(&mut bytes).iter().map(|p| (read_u32(&p[5..]).unwrap(), read_u32(&p[9..]).unwrap())).collect()
}

fn jail() -> Jail {
  Jail::new(ROOT)
}

#[test]
fn requests_inside_the_root_pass_unchanged() {
  let mut bytes = paths(3, 1, &[&format!("{ROOT}/auth.json")]);
  bytes.extend(paths(17, 2, &[ROOT]));
  bytes.extend(paths(16, 3, &["."]));
  bytes.extend(paths(18, 4, &[&format!("{ROOT}/a.tmp"), &format!("{ROOT}/sessions/a")]));

  let filtered = jail().inbound(&bytes);

  assert_eq!(filtered.forward, bytes);
  assert!(filtered.replies.is_empty());
}

#[test]
fn a_path_outside_the_root_is_refused_without_reaching_the_server() {
  let mut bytes = paths(3, 1, &["/home/user/.ssh/id_ed25519"]);
  bytes.extend(paths(11, 2, &[&format!("{ROOT}/../../..")]));
  bytes.extend(paths(17, 3, &["../other-run"]));
  bytes.extend(paths(18, 4, &[&format!("{ROOT}/a"), "/tmp/a"]));
  let ok = paths(7, 5, &[&format!("{ROOT}/x")]);
  bytes.extend(&ok);

  let filtered = jail().inbound(&bytes);

  assert_eq!(filtered.forward, ok);
  assert_eq!(refused(&filtered), [(1, 3), (2, 3), (3, 3), (4, 3)]);
}

#[test]
fn a_packet_split_across_reads_is_judged_whole() {
  let bytes = paths(3, 9, &["/etc/passwd"]);
  let jail = jail();

  let first = jail.inbound(&bytes[..7]);
  let second = jail.inbound(&bytes[7..]);

  assert!(first.forward.is_empty() && first.replies.is_empty());
  assert!(second.forward.is_empty());
  assert_eq!(refused(&second), [(9, 3)]);
}

#[test]
fn a_symbolic_link_may_point_inside_the_root_only_downwards_or_absolutely() {
  // OpenSSH's order: the link's target first, then the link's path.
  let link = format!("{ROOT}/current");
  let mut ok = paths(20, 1, &["sessions/s1", &link]);
  ok.extend(paths(20, 2, &[&format!("{ROOT}/sessions/s1"), &link]));
  let mut bad = paths(20, 3, &["../escape", &link]);
  bad.extend(paths(20, 4, &["sessions/../../x", &link]));
  bad.extend(paths(20, 5, &["/etc", &link]));
  bad.extend(paths(20, 6, &["ok", "/tmp/link"]));
  let mut bytes = ok.clone();
  bytes.extend(&bad);

  let filtered = jail().inbound(&bytes);

  assert_eq!(filtered.forward, ok);
  assert_eq!(refused(&filtered), [(3, 3), (4, 3), (5, 3), (6, 3)]);
}

#[test]
fn only_known_extensions_pass_and_their_paths_are_checked() {
  let ok = extended(1, "posix-rename@openssh.com", &[&format!("{ROOT}/a"), &format!("{ROOT}/b")]);
  let mut bytes = ok.clone();
  bytes.extend(extended(2, "hardlink@openssh.com", &["/etc/shadow", &format!("{ROOT}/s")]));
  bytes.extend(extended(3, "home-directory", &["user"]));

  let filtered = jail().inbound(&bytes);

  assert_eq!(filtered.forward, ok);
  assert_eq!(refused(&filtered), [(2, 3), (3, 8)]);
}

#[test]
fn replies_from_the_server_pass_unchanged() {
  let reply = needy_common::sftp::status_reply(4, 0, "ok");

  let out = jail().outbound(&reply);

  assert_eq!(out.now, reply);
  assert!(out.later.is_empty());
}
