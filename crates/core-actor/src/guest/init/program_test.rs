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
// Before the fix, pi, opencode drew no 24-bit color in an
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

// ROOT CAUSE:
//
// If an image put the run's user in groups besides its own (Dev Container
// features add it to `nvm`, `python` and `pipx`), the driving program had
// none of them, because init cleared the supplementary groups when it
// switched to the user (`setgroups(&[])`, and `Command::uid`, which does the
// same), so a group-writable directory such as nvm's was not writable.
//
// Before the fix, the program's `Groups:` were empty.
// The fix gives it the groups whose member lists name the user.
#[test]
fn a_program_started_as_the_user_keeps_its_supplementary_groups() {
  // SAFETY: `geteuid` cannot fail.
  if unsafe { libc::geteuid() } != 0 {
    eprintln!("skipped: switching to another user needs root (the guest tests run as root on Linux)");
    return;
  }
  let groups = [
    nix::unistd::Gid::from_raw(4242),
    nix::unistd::Gid::from_raw(4243),
  ];
  let mut command = std::process::Command::new("/bin/sh");
  command.args(["-c", "grep '^Groups:' /proc/self/status"]);
  // SAFETY: `switch_user` makes only async-signal-safe calls.
  unsafe {
    use std::os::unix::process::CommandExt;
    command.pre_exec(move || switch_user(nix::unistd::Uid::from_raw(4241), nix::unistd::Gid::from_raw(4241), &groups).map_err(Into::into));
  }

  let out = command.output().unwrap();

  let status = String::from_utf8_lossy(&out.stdout);
  assert!(out.status.success(), "{status} {}", String::from_utf8_lossy(&out.stderr));
  let listed: Vec<&str> = status.trim_start_matches("Groups:").split_whitespace().collect();
  assert_eq!(listed, ["4242", "4243"], "{status}");
}

#[test]
fn as_user_takes_the_supplementary_groups_before_the_gate() {
  let args: Vec<String> = ["1000", "1000", "999,998", "", "--", "/bin/true"].iter().map(|a| a.to_string()).collect();

  let parsed = parse_as_user(&args).unwrap();

  assert_eq!(
    parsed.groups,
    [
      nix::unistd::Gid::from_raw(999),
      nix::unistd::Gid::from_raw(998)
    ]
  );
  assert_eq!(parsed.gate, "");
  assert_eq!(parsed.program, "/bin/true");
}
