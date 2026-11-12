//! Child processes with deadlines: every command the harness runs ends,
//! whether the product under test answers or not.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

/// What a finished command printed and how it ended.
#[derive(Debug, Clone, Default)]
pub struct Output {
  /// The exit code; `None` when a signal ended it or the deadline did.
  pub code: Option<i32>,
  pub stdout: String,
  pub stderr: String,
  /// The deadline passed and the harness killed the command.
  pub timed_out: bool,
}

impl Output {
  pub fn ok(&self) -> bool {
    self.code == Some(0)
  }

  /// One line for a check's detail: the exit and the end of both streams.
  pub fn brief(&self) -> String {
    let tail = |s: &str| {
      let s = s.trim();
      let start = s.char_indices().rev().nth(400).map(|(i, _)| i).unwrap_or(0);
      s[start..].replace('\n', " | ")
    };
    let end = match (self.timed_out, self.code) {
      (true, _) => "timed out".to_string(),
      (false, Some(code)) => format!("exit {code}"),
      (false, None) => "killed by a signal".to_string(),
    };
    format!("{end}; stdout: {}; stderr: {}", tail(&self.stdout), tail(&self.stderr))
  }
}

/// Runs `command` to its end or until `timeout`, then kills it. Its stdin
/// is empty.
pub fn run(mut command: Command, timeout: Duration) -> Result<Output> {
  command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
  let shown = format!("{command:?}");
  let mut child = command.spawn().with_context(|| format!("cannot start {shown}"))?;
  let mut stdout = child.stdout.take().context("no stdout")?;
  let mut stderr = child.stderr.take().context("no stderr")?;
  // Both pipes are drained on their own threads, so a chatty child never
  // blocks on a full pipe while the harness waits for it.
  let out = thread::spawn(move || {
    let mut s = Vec::new();
    // A read error leaves what was read; the exit tells the rest.
    let _ = stdout.read_to_end(&mut s);
    s
  });
  let err = thread::spawn(move || {
    let mut s = Vec::new();
    let _ = stderr.read_to_end(&mut s);
    s
  });
  let deadline = Instant::now() + timeout;
  let mut timed_out = false;
  let status = loop {
    if let Some(status) = child.try_wait()? {
      break Some(status);
    }
    if Instant::now() >= deadline {
      timed_out = true;
      // The child may have exited in between; either way it is gone.
      let _ = child.kill();
      break child.wait().ok();
    }
    thread::sleep(Duration::from_millis(20));
  };
  let stdout = String::from_utf8_lossy(&out.join().unwrap_or_default()).into_owned();
  let stderr = String::from_utf8_lossy(&err.join().unwrap_or_default()).into_owned();
  Ok(Output {
    code: if timed_out {
      None
    } else {
      status.and_then(|s| s.code())
    },
    stdout,
    stderr,
    timed_out,
  })
}

/// `program args...` to its end, with `timeout`.
pub fn simple(program: &str, args: &[&str], timeout: Duration) -> Result<Output> {
  let mut command = Command::new(program);
  command.args(args);
  run(command, timeout)
}

/// Whether `program` is on `PATH`.
pub fn on_path(program: &str) -> bool {
  std::env::var_os("PATH").is_some_and(|paths| std::env::split_paths(&paths).any(|dir| is_executable(&dir.join(program))))
}

fn is_executable(path: &Path) -> bool {
  use std::os::unix::fs::PermissionsExt;
  path.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Every process on this machine whose command line contains `needle`,
/// as `(pid, command line)`, without the harness itself.
pub fn processes_with(needle: &str) -> Vec<(i32, String)> {
  // `ps -A -ww -o pid=,command=` reads the full command line on Linux
  // (procps) and macOS (BSD ps) alike.
  let Ok(out) = simple("ps", &["-A", "-ww", "-o", "pid=,command="], Duration::from_secs(10)) else {
    return Vec::new();
  };
  let me = std::process::id() as i32;
  out
    .stdout
    .lines()
    .filter_map(|line| {
      let line = line.trim_start();
      let (pid, command) = line.split_once(' ')?;
      let pid: i32 = pid.parse().ok()?;
      (pid != me && command.contains(needle) && !command.starts_with("ps ")).then(|| (pid, command.trim().to_string()))
    })
    .collect()
}

/// Sends `signal` to `pid`; a process that is gone already is not an error.
pub fn signal(pid: i32, signal: nix::sys::signal::Signal) {
  // ESRCH: it exited already, which is what the caller wanted.
  let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), signal);
}

/// Whether `pid` still runs (or is a zombie nobody reaped).
pub fn alive(pid: i32) -> bool {
  nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
}
