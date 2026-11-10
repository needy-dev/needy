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
// Source: https://pubs.opengroup.org/onlinepubs/9799919799/utilities/kill.html#tag_20_64_04
// (POSIX.1-2024, XSI: 1 SIGHUP, 2 SIGINT, 3 SIGQUIT, 9 SIGKILL, 15 SIGTERM).
// Reference: https://www.gnu.org/software/bash/manual/html_node/Exit-Status.html
// ("128+N"; POSIX only says greater than 128,
// https://pubs.opengroup.org/onlinepubs/9799919799/utilities/V3_chap02.html#tag_19_08_02).
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
// TODO(provenance): no POSIX or bash manual text reserves 255 for an
// out-of-range status; the only source found is the non-normative Advanced
// Bash-Scripting Guide (https://tldp.org/LDP/abs/html/exitcodes.html, read
// 2026-11-10).
// REVIEW: `shell_code` passes an exit code through unchanged, so a command
// that runs `exit 255` also reports 255; "no exit maps to here" holds only for
// signals.
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
