use std::os::unix::fs::MetadataExt;

use super::*;
use crate::guest::init::testing::other_group;

// ROOT CAUSE:
//
// If a linux-container actor's driving program ran on a terminal and the
// daemon was not root, the daemon could not connect to the process service
// (`the actor is not ready: ... transport error`), because init, as root,
// bound `process.sock` in the bind-mounted channels with root's owner and
// mode 0755, and connecting to a Unix socket needs write permission on it.
//
// Before the fix, the socket kept init's owner and the umask's mode.
// The fix gives the socket to the daemon's user and group from the guest
// configuration, mode 0600, right after it is bound.
#[tokio::test]
async fn the_process_socket_belongs_to_the_daemon_and_only_the_daemon() {
  use std::os::unix::fs::PermissionsExt;
  // The test runs as a user that is not root, so it stands for the daemon
  // with its own user and another of its groups.
  let Some(gid) = other_group() else {
    eprintln!("skipped: this user has no group besides its own to give files to");
    return;
  };
  // SAFETY: `geteuid` cannot fail.
  let uid = unsafe { libc::geteuid() };
  let dir = tempfile::tempdir().unwrap();
  let path = dir.path().join("process.sock");

  let _listener = bind_for_daemon(&path, uid, gid).unwrap();

  let meta = std::fs::metadata(&path).unwrap();
  assert_eq!((meta.uid(), meta.gid()), (uid, gid));
  assert_eq!(meta.permissions().mode() & 0o777, 0o600);
}

// ROOT CAUSE:
//
// If the driving program was a TUI that picks its palette from COLORTERM,
// it drew in 256 colors through needy although the client's terminal and
// the terminal model both keep 24-bit colors, because init cleared the
// environment and set TERM but not COLORTERM.
//
// Before the fix, pi, opencode and Claude Code drew no 24-bit color in an
// actor (the TUI matrix's `create` path against `docker run -it`).
// The fix keeps COLORTERM=truecolor, which a profile's `actor.env` can
// still override.
#[test]
fn the_driving_program_is_told_its_terminal_draws_24_bit_color() {
  let colorterm =
    |config: &GetConfigResponse| environment(config, "/home/user").into_iter().rev().find(|(k, _)| k == "COLORTERM").map(|(_, v)| v);
  assert_eq!(colorterm(&GetConfigResponse::default()).as_deref(), Some("truecolor"));

  let config = GetConfigResponse {
    env: [("COLORTERM".to_string(), "24bit".to_string())].into(),
    ..GetConfigResponse::default()
  };
  assert_eq!(colorterm(&config).as_deref(), Some("24bit"));
}
