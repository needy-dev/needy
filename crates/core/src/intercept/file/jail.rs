//! A `file.open` interceptor that keeps an SFTP stream inside one
//! directory: every request whose path lies outside the root is answered
//! with an error and never reaches `sftp-server`, which itself serves every
//! path its account can reach. The daemon installs it on the mounts of sync
//! mappings' per-run copies, whose `sftp-server` runs on the control host.
//!
//! Pure bytes, like [`crate::chaos::sftp`]: requests are framed whole,
//! judged by their paths, and passed on unchanged or refused.

use std::sync::Mutex;

use needy_common::sftp::{read_u32, status_reply, take_packets};

use crate::intercept::file::{Filtered, Later, Outbound, StreamFilter};

// Source: https://github.com/openssh/openssh-portable/blob/V_10_6_P2/sftp.h
// (`SSH2_FXP_*` request types 1 and 3-20, `SSH2_FXP_EXTENDED 200`;
// `SSH2_FX_PERMISSION_DENIED 3`, `SSH2_FX_OP_UNSUPPORTED 8`).
// Reference: https://datatracker.ietf.org/doc/html/draft-ietf-secsh-filexfer-02#section-6
// (the requests and the paths each carries).
// TODO(provenance): line anchors in `sftp.h` at V_10_6_P2.
const INIT: u8 = 1;
const OPEN: u8 = 3;
const CLOSE: u8 = 4;
const READ: u8 = 5;
const WRITE: u8 = 6;
const LSTAT: u8 = 7;
const FSTAT: u8 = 8;
const SETSTAT: u8 = 9;
const FSETSTAT: u8 = 10;
const OPENDIR: u8 = 11;
const READDIR: u8 = 12;
const REMOVE: u8 = 13;
const MKDIR: u8 = 14;
const RMDIR: u8 = 15;
const REALPATH: u8 = 16;
const STAT: u8 = 17;
const RENAME: u8 = 18;
const READLINK: u8 = 19;
const SYMLINK: u8 = 20;
const EXTENDED: u8 = 200;
const PERMISSION_DENIED: u32 = 3;
const OP_UNSUPPORTED: u32 = 8;

/// What a request asks of the file system, for the jail to judge.
enum Request {
  /// Carries no path: handles, or the version handshake.
  Free,
  /// Carries these paths.
  Paths(Vec<String>),
  /// A symbolic link: its target, then its own path.
  Link { target: String, path: String },
  /// Something the jail does not know, which it refuses.
  Unknown,
}

/// Keeps one SFTP stream inside `root`.
pub struct Jail {
  root: String,
  to_server: Mutex<Vec<u8>>,
}

impl Jail {
  /// The jail of a stream whose `sftp-server` runs in `root`, the
  /// directory relative paths start from.
  pub fn new(root: &str) -> Self {
    Self {
      root: normalize(root),
      to_server: Mutex::new(Vec::new()),
    }
  }

  /// Whether `path`, absolute or relative to the root, names the root or
  /// something under it, without following links.
  fn inside(&self, path: &str) -> bool {
    let absolute = if path.starts_with('/') {
      normalize(path)
    } else {
      normalize(&format!("{}/{path}", self.root))
    };
    absolute == self.root || absolute.starts_with(&format!("{}/", self.root.trim_end_matches('/')))
  }

  /// Whether a symbolic link at `path` to `target` stays inside: a target
  /// inside the root by its absolute path, or a relative one that only
  /// goes down, so moving the link never makes it point out.
  fn link_inside(&self, target: &str, path: &str) -> bool {
    let target_inside = if target.starts_with('/') {
      self.inside(target)
    } else {
      !target.split('/').any(|part| part == "..")
    };
    target_inside && self.inside(path)
  }

  fn allows(&self, packet: &[u8]) -> Result<(), u32> {
    match request(packet) {
      Request::Free => Ok(()),
      Request::Paths(paths) if paths.iter().all(|p| self.inside(p)) => Ok(()),
      Request::Link { target, path } if self.link_inside(&target, &path) => Ok(()),
      Request::Paths(_) | Request::Link { .. } => Err(PERMISSION_DENIED),
      Request::Unknown => Err(OP_UNSUPPORTED),
    }
  }
}

impl StreamFilter for Jail {
  fn inbound(&self, bytes: &[u8]) -> Filtered {
    let mut buffer = self.to_server.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    buffer.extend_from_slice(bytes);
    let mut filtered = Filtered::default();
    for packet in take_packets(&mut buffer) {
      match self.allows(&packet) {
        Ok(()) => filtered.forward.extend(packet),
        Err(code) => {
          let id = read_u32(packet.get(5..).unwrap_or_default()).unwrap_or(0);
          filtered.replies.push(Later {
            after: std::time::Duration::ZERO,
            bytes: status_reply(id, code, "outside the mapping"),
          });
        }
      }
    }
    filtered
  }

  fn outbound(&self, bytes: &[u8]) -> Outbound {
    Outbound {
      now: bytes.to_vec(),
      later: Vec::new(),
    }
  }
}

/// The request in a whole packet.
fn request(packet: &[u8]) -> Request {
  let Some(&kind) = packet.get(4) else {
    return Request::Unknown;
  };
  // After the length, the type and the request ID.
  let mut body = Reader(packet.get(9..).unwrap_or_default());
  let strings = |body: &mut Reader, n: usize| -> Option<Vec<String>> { (0..n).map(|_| body.string()).collect() };
  let parsed = match kind {
    INIT | CLOSE | READ | WRITE | FSTAT | FSETSTAT | READDIR => return Request::Free,
    OPEN | LSTAT | SETSTAT | OPENDIR | REMOVE | MKDIR | RMDIR | REALPATH | STAT | READLINK => strings(&mut body, 1).map(Request::Paths),
    RENAME => strings(&mut body, 2).map(Request::Paths),
    // NOTICE: OpenSSH's `sftp-server` reads SYMLINK's two paths in the
    // other order than the draft, the target first, and sshfs sends them
    // so (`process_symlink` in
    // https://github.com/openssh/openssh-portable/blob/V_10_6_P2/sftp-server.c
    // and `sshfs_symlink` in
    // https://github.com/libfuse/sshfs/blob/sshfs-3.7.6/sshfs.c).
    // TODO(provenance): line anchors for both functions.
    SYMLINK => strings(&mut body, 2).map(|mut both| {
      let path = both.pop().unwrap_or_default();
      let target = both.pop().unwrap_or_default();
      Request::Link { target, path }
    }),
    EXTENDED => body.string().and_then(|name| extension(&name, &mut body)),
    _ => None,
  };
  parsed.unwrap_or(Request::Unknown)
}

/// The OpenSSH extensions the jail lets through, with their paths; any
/// other extension is refused.
// Source: https://github.com/openssh/openssh-portable/blob/V_10_6_P2/PROTOCOL
// (section 4, the `sftp-server` extensions and their arguments).
// TODO(provenance): line anchors in `PROTOCOL` at V_10_6_P2.
fn extension(name: &str, body: &mut Reader) -> Option<Request> {
  let paths = |body: &mut Reader, n: usize| (0..n).map(|_| body.string()).collect::<Option<Vec<String>>>().map(Request::Paths);
  match name {
    "posix-rename@openssh.com" | "hardlink@openssh.com" => paths(body, 2),
    "statvfs@openssh.com" | "lsetstat@openssh.com" => paths(body, 1),
    "fstatvfs@openssh.com" | "fsync@openssh.com" | "limits@openssh.com" => Some(Request::Free),
    _ => Some(Request::Unknown),
  }
}

/// `path` with `.` and `..` resolved by its text, and no trailing `/`.
fn normalize(path: &str) -> String {
  let mut parts: Vec<&str> = Vec::new();
  for part in path.split('/') {
    match part {
      "" | "." => {}
      ".." => {
        parts.pop();
      }
      part => parts.push(part),
    }
  }
  format!("/{}", parts.join("/"))
}

/// Reads SFTP's `string`s: a `uint32` length and that many bytes.
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
  fn string(&mut self) -> Option<String> {
    let len = read_u32(self.0)? as usize;
    let bytes = self.0.get(4..4 + len)?;
    self.0 = &self.0[4 + len..];
    String::from_utf8(bytes.to_vec()).ok()
  }
}

#[cfg(test)]
#[path = "jail_test.rs"]
mod tests;
