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

/// Shell-style status a client on this machine exits with for `exit`: the
/// exit code, or 128 plus the signal number. A code this machine's exit
/// status cannot carry, such as a Windows code above 255 seen on Unix, is
/// [`OUT_OF_RANGE`]; [`unrepresentable`] names it for the client's message.
pub fn shell_code(exit: &Exit) -> i32 {
  match exit.code {
    Some(code) if fits(code) => code,
    Some(_) => OUT_OF_RANGE,
    None => 128 + number(&exit.signal).unwrap_or(1),
  }
}

/// The status for an exit code outside what an exit status carries here: a
/// failure, never the code's low bits, which may be 0.
pub const OUT_OF_RANGE: i32 = 1;

/// The code of `exit` when [`shell_code`] cannot pass it on unchanged.
pub fn unrepresentable(exit: &Exit) -> Option<i32> {
  exit.code.filter(|code| !fits(*code))
}

/// What a client prints when [`unrepresentable`] names a code.
pub fn out_of_range_message(code: i32) -> String {
  format!("the command exited with code {code}, which an exit status here cannot carry; exiting with {OUT_OF_RANGE}")
}

/// Whether an exit status on this machine carries `code` whole.
// Source: https://pubs.opengroup.org/onlinepubs/9799919799/functions/_exit.html#tag_17_02_03
// (POSIX.1-2024, `_exit`: "only the least significant 8 bits (that is,
// status & 0377) shall be available from wait() and waitpid()").
// Reference: https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-exitprocess
// (a Windows exit code is a 32-bit `UINT`, so it fits whole there).
#[cfg(unix)]
fn fits(code: i32) -> bool {
  (0..=255).contains(&code)
}

#[cfg(not(unix))]
fn fits(_code: i32) -> bool {
  true
}

/// The status a client reports when an exec stream ended before the command
/// exited, so the command's own status is unknown. No signal maps to it here,
/// but a command that runs `exit 255` also reports 255, so the status alone
/// does not tell the two apart; the client's message does.
// NOTICE: 255 is needy's convention, not a reserved value. What constrains
// it: a wait status carries only the low 8 bits of an exit status, so 255 is
// the largest one a client can report
// (https://pubs.opengroup.org/onlinepubs/9799919799/functions/exit.html,
// POSIX.1-2024 `exit`), and ssh(1) also exits 255 when it, not the remote
// command, failed
// (https://github.com/openssh/openssh-portable/blob/V_10_6_P2/ssh.1#L1578-L1581).
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
