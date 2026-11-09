//! Unix processes: each child leads its own process group, signals go to the
//! group, and a terminal's child is reaped with `waitpid` so a signal death
//! keeps its exact signal.

use needy_common::process::Exit;
use nix::sys::signal::{Signal, kill, killpg};
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::Pid;

use crate::signals;

/// Makes a piped child lead a new process group, so a signal reaches the
/// commands it starts too. A terminal's child leads its own session already.
pub fn own_group(command: &mut tokio::process::Command) {
  command.process_group(0);
}

pub fn exit_from_std(status: std::process::ExitStatus) -> Exit {
  use std::os::unix::process::ExitStatusExt;
  match status.signal() {
    Some(sig) => Exit {
      code: None,
      signal: signals::name(sig),
    },
    None => Exit {
      code: status.code(),
      signal: String::new(),
    },
  }
}

/// Waits for a terminal's child by reaping it directly; portable-pty's own
/// wait reports a signal death only as a description.
pub fn wait_terminal(_child: &mut Box<dyn portable_pty::Child + Send + Sync>, pid: i64) -> Exit {
  loop {
    match waitpid(Pid::from_raw(pid as i32), None) {
      Ok(WaitStatus::Exited(_, code)) => {
        return Exit {
          code: Some(code),
          signal: String::new(),
        };
      }
      Ok(WaitStatus::Signaled(_, sig, _)) => {
        return Exit {
          code: None,
          signal: sig.as_str().to_string(),
        };
      }
      Ok(_) | Err(nix::errno::Errno::EINTR) => continue,
      Err(e) => {
        tracing::warn!("process {pid}: waitpid: {e}");
        return super::proc::lost();
      }
    }
  }
}

/// Sends the signal `name` to the process group `pid` leads, or to the
/// process alone when it has left its group. An unknown name is ignored.
pub fn signal_group(pid: i64, name: &str) {
  let Ok(sig) = name.parse::<Signal>() else {
    tracing::debug!("process {pid}: unknown signal {name}");
    return;
  };
  if pid <= 0 {
    return;
  }
  let pid = Pid::from_raw(pid as i32);
  if killpg(pid, sig).is_err()
    && let Err(e) = kill(pid, sig)
  {
    // The process has exited already; its exit reports what happened.
    tracing::debug!("process {pid}: {name}: {e}");
  }
}
