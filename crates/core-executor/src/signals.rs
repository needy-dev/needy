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
mod tests {
  use super::*;

  #[test]
  fn names_and_numbers() {
    assert_eq!(number("SIGTERM"), Some(15));
    assert_eq!(name(9), "SIGKILL");
    assert_eq!(parse("TERM").as_deref(), Some("SIGTERM"));
    assert_eq!(parse("2").as_deref(), Some("SIGINT"));
    assert_eq!(parse("NOPE"), None);
    let exit = Exit {
      code: None,
      signal: "SIGTERM".into(),
    };
    assert_eq!(shell_code(&exit), 143);
  }

  #[cfg(unix)]
  #[test]
  fn platform_numbers_differ() {
    // SIGUSR1 is 10 on Linux and 30 on macOS; names are what travel.
    let usr1 = number("SIGUSR1").unwrap();
    assert_eq!(name(usr1), "SIGUSR1");
  }
}
