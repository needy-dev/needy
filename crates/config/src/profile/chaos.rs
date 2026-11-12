//! Chaos: named, deterministic faults. Each fault names one domain, the
//! subject it matches there, the effects it has and when it fires.
//!
//! ```yaml
//! chaos:
//!   seed: 1
//!   faults:
//!     curl-down: { exec: "curl **", exit: 7, times: 3 }
//!     flaky-json: { file: { open: "**/*.json" }, errno: EIO, probability: 0.1 }
//!     slow-example: { net: { connect: "*.example.com" }, delay: 2s }
//!     overheat: { sensor: { temp_c: 101 }, at: 30s }
//! ```
//!
//! A fault is read through [`Spec`], which checks that it names exactly one
//! domain and only that domain's effects and triggers, so a fault that loads
//! is one needy can deliver.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::value::{Duration, Glob};

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Chaos {
  /// Mixed with each fault's name and match count, so a replay with the
  /// same seed fires the same faults.
  pub seed: u64,
  /// Faults by name. They are checked in name order; the first that fires
  /// on an operation is the one it gets.
  pub faults: BTreeMap<String, Fault>,
}

/// The domain a fault applies in, as `fault.injected` and the report name
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FaultDomain {
  /// A command, at `exec.decide`.
  Exec,
  /// A file the actor opens on a workspace mount, at `file.open`.
  File,
  /// A connection or plain-HTTP request at the actor gateway, at
  /// `net.connect` and `net.request`.
  Net,
  /// The actor's sensors, at a time after the actor starts.
  Sensor,
}

impl FaultDomain {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Exec => "exec",
      Self::File => "file",
      Self::Net => "net",
      Self::Sensor => "sensor",
    }
  }
}

impl fmt::Display for FaultDomain {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

/// One fault, as written. Exactly one of `exec`, `file`, `net` and `sensor`
/// is set, and only that domain's effects and triggers (see [`Spec`]).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Spec")]
pub struct Fault {
  /// A command pattern, written like a rule's `match`: `curl **`.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub exec: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub file: Option<FileMatch>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub net: Option<NetMatch>,
  /// The readings a sensor fault sets.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub sensor: Option<Readings>,

  /// exec: the command exits with this code without running.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub exit: Option<i32>,
  /// exec: the command dies of this signal without running, by name
  /// (`SIGSEGV`).
  #[serde(skip_serializing_if = "Option::is_none")]
  pub signal: Option<String>,
  /// exec: written to the command's stderr before its `exit`, `signal` or
  /// `hang`.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub stderr: Option<String>,
  /// file: the open fails with this errno.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub errno: Option<String>,
  /// net: the connection is refused, as by a host with nothing listening.
  #[serde(skip_serializing_if = "is_false")]
  pub refuse: bool,
  /// net: the connection is closed without an answer.
  #[serde(skip_serializing_if = "is_false")]
  pub reset: bool,
  /// net: a plain-HTTP request (`net: { request: ... }`) is answered with
  /// this status.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub status: Option<u16>,
  /// exec and net: the operation never completes, until its caller gives
  /// up or the run ends.
  #[serde(skip_serializing_if = "is_false")]
  pub hang: bool,
  /// exec, file and net: the operation is held this long first.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub delay: Option<Duration>,

  /// Fire with this probability on each match, from the seed.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub probability: Option<f64>,
  /// Fire only on this match, counted from 1.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub nth: Option<u64>,
  /// Fire at most this many times.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub times: Option<u64>,
  /// Match only this long after the run was created, or later.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub after: Option<Duration>,
  /// Match only until this long after the run was created.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub until: Option<Duration>,
  /// sensor: when the readings are set, after the actor starts.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub at: Option<Duration>,
}

fn is_false(value: &bool) -> bool {
  !value
}

/// What a file fault matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileMatch {
  /// A glob on the path the actor opens, as the actor sees it (`~`
  /// expanded): `**/*.json`, `~/workspace/src/**`.
  pub open: String,
}

/// What a net fault matches: exactly one of the two.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetMatch {
  /// Every connection to a matching host: a `CONNECT` tunnel or a
  /// plain-HTTP request. A glob matched like `network.allow`.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub connect: Option<Glob>,
  /// Plain-HTTP requests to a matching host only, such as a service's.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub request: Option<Glob>,
}

/// Sensor readings a sensor fault sets.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Readings {
  /// Package temperature in degrees Celsius.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub temp_c: Option<f64>,
}

/// The errnos a file fault can deliver. A file fault answers the open of an
/// sshfs mount on its SFTP stream, and sshfs turns SFTP status codes into
/// errnos: these four by their own code, and every other code into `EIO`.
// Source: https://github.com/libfuse/sshfs/blob/sshfs-3.7.6/sshfs.c#L1766-L1779
// (`sftp_error_to_errno`: 2 ENOENT, 3 EACCES, 4 EPERM, 8 EOPNOTSUPP, any
// other EIO); `needy_core::chaos::sftp` holds the inverse table.
pub const FILE_ERRNOS: [&str; 5] = ["ENOENT", "EACCES", "EPERM", "EOPNOTSUPP", "EIO"];

/// The signals an exec fault may end a command with: the standard signals
/// whose default action ends the process (`Term` or `Core`).
// Source: https://git.kernel.org/pub/scm/docs/man-pages/man-pages.git/tree/man/man7/signal.7?h=man-pages-6.9
// (the "Standard signals" table of signal(7), man-pages 6.9).
// Reference: https://man7.org/linux/man-pages/man7/signal.7.html (read
// 2026-11-12).
// NOTICE: SIGCHLD, SIGCONT, SIGURG and SIGWINCH are ignored by default and
// SIGSTOP, SIGTSTP, SIGTTIN and SIGTTOU stop the process, so none of them
// ends a command; they are left out.
pub const SIGNALS: [&str; 23] = [
  "SIGHUP",
  "SIGINT",
  "SIGQUIT",
  "SIGILL",
  "SIGTRAP",
  "SIGABRT",
  "SIGBUS",
  "SIGFPE",
  "SIGKILL",
  "SIGUSR1",
  "SIGSEGV",
  "SIGUSR2",
  "SIGPIPE",
  "SIGALRM",
  "SIGTERM",
  "SIGSTKFLT",
  "SIGXCPU",
  "SIGXFSZ",
  "SIGVTALRM",
  "SIGPROF",
  "SIGIO",
  "SIGPWR",
  "SIGSYS",
];

impl Fault {
  /// The domain this fault applies in.
  pub fn domain(&self) -> FaultDomain {
    match (&self.exec, &self.file, &self.net) {
      (Some(_), _, _) => FaultDomain::Exec,
      (_, Some(_), _) => FaultDomain::File,
      (_, _, Some(_)) => FaultDomain::Net,
      _ => FaultDomain::Sensor,
    }
  }

  /// Expands a leading `~` in a file fault's glob to the profile user's
  /// home: the glob matches the path in the actor.
  pub fn expand_home(&mut self, user: &super::User) {
    if let Some(file) = &mut self.file {
      file.open = user.expand_home(&file.open);
    }
  }

  /// Whether the fault has no trigger of its own and so fires on every
  /// match (or, for a sensor, at once).
  pub fn always(&self) -> bool {
    self.probability.is_none() && self.nth.is_none() && self.times.is_none() && self.after.is_none() && self.until.is_none()
  }

  /// Checks that the fault names exactly one domain and only what that
  /// domain can deliver, and returns it with its signal spelled `SIGNAME`.
  pub fn checked(mut self) -> Result<Self, String> {
    let domains: Vec<&str> = [
      ("exec", self.exec.is_some()),
      ("file", self.file.is_some()),
      ("net", self.net.is_some()),
      ("sensor", self.sensor.is_some()),
    ]
    .into_iter()
    .filter_map(|(name, set)| set.then_some(name))
    .collect();
    match domains.as_slice() {
      [] => return Err("a fault names one domain: `exec`, `file`, `net` or `sensor`".into()),
      [_] => {}
      many => return Err(format!("a fault names one domain, not {}", many.join(" and "))),
    }
    let domain = self.domain();
    let set: Vec<&'static str> = self.keys().into_iter().filter_map(|(key, set)| set.then_some(key)).collect();
    let allowed: &[&str] = match domain {
      FaultDomain::Exec => &[
        "exit",
        "signal",
        "stderr",
        "hang",
        "delay",
        "probability",
        "nth",
        "times",
        "after",
        "until",
      ],
      FaultDomain::File => &[
        "errno",
        "delay",
        "probability",
        "nth",
        "times",
        "after",
        "until",
      ],
      FaultDomain::Net => &[
        "refuse",
        "reset",
        "status",
        "hang",
        "delay",
        "probability",
        "nth",
        "times",
        "after",
        "until",
      ],
      FaultDomain::Sensor => &["at"],
    };
    if let Some(key) = set.iter().find(|key| !allowed.contains(key)) {
      return Err(format!("{key} is not for {domain} faults; {}", Self::offers(domain)));
    }
    match domain {
      FaultDomain::Exec => self.check_exec()?,
      FaultDomain::File => self.check_file()?,
      FaultDomain::Net => self.check_net()?,
      FaultDomain::Sensor => {}
    }
    if let Some(p) = self.probability
      && !(0.0..=1.0).contains(&p)
    {
      return Err(format!("probability {p} is not between 0 and 1"));
    }
    if self.nth == Some(0) {
      return Err("nth counts matches from 1".into());
    }
    if self.times == Some(0) {
      return Err("times: 0 would never fire".into());
    }
    if self.nth.is_some() && (self.probability.is_some() || self.times.is_some()) {
      return Err("nth fires on one match; it takes no probability or times".into());
    }
    if let (Some(after), Some(until)) = (self.after, self.until)
      && until <= after
    {
      return Err("until must be later than after".into());
    }
    Ok(self)
  }

  /// Every effect and trigger key, and whether it is set.
  fn keys(&self) -> [(&'static str, bool); 15] {
    [
      ("exit", self.exit.is_some()),
      ("signal", self.signal.is_some()),
      ("stderr", self.stderr.is_some()),
      ("errno", self.errno.is_some()),
      ("refuse", self.refuse),
      ("reset", self.reset),
      ("status", self.status.is_some()),
      ("hang", self.hang),
      ("delay", self.delay.is_some()),
      ("probability", self.probability.is_some()),
      ("nth", self.nth.is_some()),
      ("times", self.times.is_some()),
      ("after", self.after.is_some()),
      ("until", self.until.is_some()),
      ("at", self.at.is_some()),
    ]
  }

  /// What a domain's faults take, for an error message.
  fn offers(domain: FaultDomain) -> &'static str {
    match domain {
      FaultDomain::Exec => "an exec fault takes exit, signal, hang or delay (and stderr), with probability, nth, times, after or until",
      FaultDomain::File => "a file fault takes errno or delay, with probability, nth, times, after or until",
      FaultDomain::Net => "a net fault takes refuse, reset, hang, status or delay, with probability, nth, times, after or until",
      FaultDomain::Sensor => "a sensor fault takes only `at`",
    }
  }

  fn check_exec(&mut self) -> Result<(), String> {
    let ends = [self.exit.is_some(), self.signal.is_some(), self.hang].into_iter().filter(|set| *set).count();
    if ends > 1 {
      return Err("an exec fault ends the command one way: exit, signal or hang".into());
    }
    if self.stderr.is_some() && ends == 0 {
      return Err("stderr is written before exit, signal or hang; add one".into());
    }
    if ends == 0 && self.delay.is_none() {
      return Err(format!("an exec fault needs an effect; {}", Self::offers(FaultDomain::Exec)));
    }
    if let Some(code) = self.exit
      && !(0..=255).contains(&code)
    {
      return Err(format!("exit {code} is not an exit status (0 to 255)"));
    }
    if let Some(signal) = &self.signal {
      let name = if signal.starts_with("SIG") {
        signal.clone()
      } else {
        format!("SIG{signal}")
      };
      if !SIGNALS.contains(&name.as_str()) {
        return Err(format!("signal {signal} does not end a process; one of {}", SIGNALS.join(", ")));
      }
      self.signal = Some(name);
    }
    if self.exec.as_deref().is_some_and(|pattern| pattern.trim().is_empty()) {
      return Err("exec needs a command pattern, such as `curl **` (`**` for every command)".into());
    }
    Ok(())
  }

  fn check_file(&self) -> Result<(), String> {
    if self.errno.is_none() && self.delay.is_none() {
      return Err(format!("a file fault needs an effect; {}", Self::offers(FaultDomain::File)));
    }
    if let Some(errno) = &self.errno
      && !FILE_ERRNOS.contains(&errno.as_str())
    {
      return Err(format!(
        "errno {errno} cannot reach the program: sshfs turns every other SFTP status into EIO; one of {}",
        FILE_ERRNOS.join(", ")
      ));
    }
    Ok(())
  }

  fn check_net(&self) -> Result<(), String> {
    let Some(net) = &self.net else {
      return Ok(());
    };
    match (&net.connect, &net.request) {
      (Some(_), Some(_)) => return Err("net matches `connect` or `request`, not both".into()),
      (None, None) => return Err("net needs `connect: HOST` or `request: HOST`".into()),
      _ => {}
    }
    let ends = [self.refuse, self.reset, self.hang, self.status.is_some()].into_iter().filter(|set| *set).count();
    if ends > 1 {
      return Err("a net fault fails the connection one way: refuse, reset, hang or status".into());
    }
    if ends == 0 && self.delay.is_none() {
      return Err(format!("a net fault needs an effect; {}", Self::offers(FaultDomain::Net)));
    }
    if let Some(status) = self.status {
      if net.request.is_none() {
        return Err("status answers a plain-HTTP request; match it with `net: { request: HOST }` (a CONNECT tunnel is opaque)".into());
      }
      if !(100..=599).contains(&status) {
        return Err(format!("status {status} is not an HTTP status"));
      }
    }
    Ok(())
  }
}

/// A fault as read, before [`Fault::checked`].
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Spec {
  #[serde(default)]
  exec: Option<String>,
  #[serde(default)]
  file: Option<FileMatch>,
  #[serde(default)]
  net: Option<NetMatch>,
  #[serde(default)]
  sensor: Option<Readings>,
  #[serde(default)]
  exit: Option<i32>,
  #[serde(default)]
  signal: Option<String>,
  #[serde(default)]
  stderr: Option<String>,
  #[serde(default)]
  errno: Option<String>,
  #[serde(default)]
  refuse: bool,
  #[serde(default)]
  reset: bool,
  #[serde(default)]
  status: Option<u16>,
  #[serde(default)]
  hang: bool,
  #[serde(default)]
  delay: Option<Duration>,
  #[serde(default)]
  probability: Option<f64>,
  #[serde(default)]
  nth: Option<u64>,
  #[serde(default)]
  times: Option<u64>,
  #[serde(default)]
  after: Option<Duration>,
  #[serde(default)]
  until: Option<Duration>,
  #[serde(default)]
  at: Option<Duration>,
}

impl TryFrom<Spec> for Fault {
  type Error = String;

  fn try_from(s: Spec) -> Result<Self, String> {
    Fault {
      exec: s.exec,
      file: s.file,
      net: s.net,
      sensor: s.sensor,
      exit: s.exit,
      signal: s.signal,
      stderr: s.stderr,
      errno: s.errno,
      refuse: s.refuse,
      reset: s.reset,
      status: s.status,
      hang: s.hang,
      delay: s.delay,
      probability: s.probability,
      nth: s.nth,
      times: s.times,
      after: s.after,
      until: s.until,
      at: s.at,
    }
    .checked()
  }
}

/// Whether `name` may name a fault: it appears in the trace, the report and
/// on `needy chaos` command lines, so it is a word of letters, digits, `-`,
/// `_` and `.`.
pub fn valid_name(name: &str) -> bool {
  !name.is_empty() && name.len() <= 64 && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}
