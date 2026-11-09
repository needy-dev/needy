//! Unix processes: each child leads its own process group, signals go to the
//! group, and a terminal's child is reaped with `waitpid` so a signal death
//! keeps its exact signal.

use needy_common::process::Exit;
use nix::sys::signal::{SigHandler, SigSet, SigmaskHow, Signal, kill, killpg, signal, sigprocmask};
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::Pid;

use crate::signals;

/// Makes a piped child lead a new process group, so a signal reaches the
/// commands it starts too. A terminal's child leads its own session already.
pub fn own_group(command: &mut tokio::process::Command) {
  command.process_group(0);
}

/// Signals a piped child gets back at their default disposition before
/// `exec`.
// NOTICE: POSIX keeps `SIG_IGN` across `exec`, so a daemon started with `&`
// from a non-interactive shell (SIGINT and SIGQUIT ignored) passed that on
// to every child, and `needy kill -s INT` did nothing. portable-pty resets
// SIGCHLD, SIGHUP, SIGINT, SIGQUIT, SIGTERM and SIGALRM and clears the mask
// for a terminal's child (`portable-pty-0.9.0/src/unix.rs`, `pre_exec` in
// `spawn_command`); the job-control signals and SIGPIPE are added here. std
// clears the mask and resets SIGPIPE itself, but nothing else.
const RESET_SIGNALS: [Signal; 10] = [
  Signal::SIGINT,
  Signal::SIGQUIT,
  Signal::SIGTERM,
  Signal::SIGHUP,
  Signal::SIGALRM,
  Signal::SIGCHLD,
  Signal::SIGTSTP,
  Signal::SIGTTIN,
  Signal::SIGTTOU,
  Signal::SIGPIPE,
];

/// Gives a piped child default signal dispositions and an empty signal mask,
/// whatever the executor inherited.
pub fn default_signals(command: &mut tokio::process::Command) {
  // SAFETY: the closure runs in the forked child before `exec` and calls
  // only `signal`, `sigemptyset` and `sigprocmask`, which are
  // async-signal-safe. It allocates nothing and takes no lock.
  unsafe {
    command.pre_exec(|| {
      for sig in RESET_SIGNALS {
        signal(sig, SigHandler::SigDfl).map_err(std::io::Error::from)?;
      }
      sigprocmask(SigmaskHow::SIG_SETMASK, Some(&SigSet::empty()), None).map_err(std::io::Error::from)?;
      Ok(())
    });
  }
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

/// The bytes that end input on the terminal behind `master`: its end-of-file
/// character (VEOF, usually `0x04`) when it reads by lines (ICANON), and
/// nothing in raw mode, where that byte would be a keystroke. A terminal
/// whose settings cannot be read gets `0x04`, as before.
pub fn end_of_input(master: &dyn portable_pty::MasterPty) -> Vec<u8> {
  use nix::sys::termios::{LocalFlags, SpecialCharacterIndices, tcgetattr};
  use std::os::fd::BorrowedFd;
  let Some(fd) = master.as_raw_fd() else {
    return vec![4];
  };
  // SAFETY: the master is borrowed for the call and stays open meanwhile.
  match tcgetattr(unsafe { BorrowedFd::borrow_raw(fd) }) {
    Ok(termios) if termios.local_flags.contains(LocalFlags::ICANON) => {
      vec![termios.control_chars[SpecialCharacterIndices::VEOF as usize]]
    }
    Ok(_) => Vec::new(),
    Err(e) => {
      tracing::debug!("terminal settings: {e}");
      vec![4]
    }
  }
}

/// Whether this machine has a terminfo entry for the terminal type `name`,
/// looked up where ncurses looks (terminfo(5)): `$TERMINFO`,
/// `~/.terminfo`, `$TERMINFO_DIRS`, then the system directories, each with
/// entries under their first letter or its hexadecimal code (macOS).
// NOTICE: written here rather than with a terminfo crate: only the entry's
// existence matters, and the crates parse the compiled database as well.
// The system directories are the ones ncurses builds and distributions use.
pub fn has_terminfo(name: &str) -> bool {
  use std::path::PathBuf;
  let Some(first) = name.chars().next() else {
    return false;
  };
  if name.contains('/') {
    return false;
  }
  let mut dirs: Vec<PathBuf> = Vec::new();
  if let Some(dir) = std::env::var_os("TERMINFO") {
    dirs.push(dir.into());
  }
  if let Some(home) = std::env::var_os("HOME") {
    dirs.push(PathBuf::from(home).join(".terminfo"));
  }
  if let Some(list) = std::env::var_os("TERMINFO_DIRS") {
    dirs.extend(std::env::split_paths(&list).filter(|d| !d.as_os_str().is_empty()));
  }
  dirs.extend(
    [
      "/etc/terminfo",
      "/lib/terminfo",
      "/usr/share/terminfo",
      "/usr/lib/terminfo",
      "/usr/local/share/terminfo",
    ]
    .map(PathBuf::from),
  );
  let letter = first.to_string();
  let hex = format!("{:x}", u32::from(first));
  dirs.iter().any(|dir| dir.join(&letter).join(name).exists() || dir.join(&hex).join(name).exists())
}

/// Waits for output on a terminal, so its reader can stop without the
/// terminal's end: a descendant in its own session may keep it open after
/// the child exits.
pub struct Readable(std::os::fd::OwnedFd);

impl Readable {
  /// Watches the terminal behind `master` through a duplicate of its file
  /// descriptor, which stays valid after the master is dropped.
  pub fn new(master: &dyn portable_pty::MasterPty) -> std::io::Result<Self> {
    use std::os::fd::BorrowedFd;
    let fd = master.as_raw_fd().ok_or_else(|| std::io::Error::other("the terminal has no file descriptor"))?;
    // SAFETY: the master is open for the length of this call.
    let duplicate = unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned()?;
    Ok(Self(duplicate))
  }

  /// Returns once the terminal has output or has ended (a read then says
  /// which), or `false` once `stop` is set.
  pub fn wait(&self, stop: &std::sync::atomic::AtomicBool) -> bool {
    use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
    use std::os::fd::AsFd;
    loop {
      if stop.load(std::sync::atomic::Ordering::SeqCst) {
        return false;
      }
      // NOTICE: 100 ms bounds how long a stop waits; it is no output delay,
      // since output ends the wait at once.
      let mut fds = [PollFd::new(self.0.as_fd(), PollFlags::POLLIN)];
      match poll(&mut fds, PollTimeout::from(100u16)) {
        Ok(0) | Err(nix::errno::Errno::EINTR) => continue,
        Ok(_) | Err(_) => return true,
      }
    }
  }
}
