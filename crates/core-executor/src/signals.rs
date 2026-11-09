//! Signal names and this platform's signal numbers. Names travel between
//! machines; numbers never do, because they differ between platforms.

use needy_common::process::Exit;

/// This platform's number for a signal name such as `SIGTERM`.
#[cfg(unix)]
pub fn number(name: &str) -> Option<i32> {
  name.parse::<nix::sys::signal::Signal>().ok().map(|s| s as i32)
}

/// The name of one of this platform's signal numbers.
#[cfg(unix)]
pub fn name(number: i32) -> String {
  nix::sys::signal::Signal::try_from(number).map(|s| s.as_str().to_string()).unwrap_or_else(|_| format!("SIG{number}"))
}

/// Windows has no POSIX signals. These numbers only produce the conventional
/// `128 + n` shell status for a process that ended by a signal elsewhere.
#[cfg(not(unix))]
pub fn number(name: &str) -> Option<i32> {
  match name {
    "SIGHUP" => Some(1),
    "SIGINT" => Some(2),
    "SIGQUIT" => Some(3),
    "SIGKILL" => Some(9),
    "SIGTERM" => Some(15),
    _ => None,
  }
}

#[cfg(not(unix))]
pub fn name(number: i32) -> String {
  format!("SIG{number}")
}

/// Shell-style status: the exit code, or 128 plus the signal number.
pub fn shell_code(exit: &Exit) -> i32 {
  match exit.code {
    Some(code) => code,
    None => 128 + number(&exit.signal).unwrap_or(1),
  }
}

/// The status a client reports when an exec stream ended before the command
/// exited, so the command's own status is unknown. A shell reserves 255 for
/// an exit status out of range, which no exit or signal maps to here.
pub const ENDED: i32 = 255;

/// Accepts `SIGTERM`, `TERM` or a number, and returns the signal name.
pub fn parse(text: &str) -> Option<String> {
  if let Ok(n) = text.parse::<i32>() {
    return Some(name(n));
  }
  let name = if text.starts_with("SIG") {
    text.to_string()
  } else {
    format!("SIG{text}")
  };
  number(&name).map(|_| name)
}

#[cfg(test)]
#[path = "signals_test.rs"]
mod tests;
