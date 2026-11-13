//! The invariants a cell asserts, and the result lines its driving program
//! prints.

use std::fmt::Debug;

/// One invariant and whether it held.
#[derive(Debug, Clone)]
pub struct Check {
  pub name: String,
  pub ok: bool,
  pub detail: String,
}

/// What a cell asserted, in order.
#[derive(Debug, Default)]
pub struct Checks {
  pub list: Vec<Check>,
  /// Why invariants the cell would assert were not: deferred work the
  /// product does not do yet. A cell with any is not covered, never passed.
  pub deferred: Vec<String>,
}

impl Checks {
  /// Records `name`: held when `ok`; `detail` says what was seen.
  pub fn check(&mut self, name: impl Into<String>, ok: bool, detail: impl Into<String>) -> bool {
    self.list.push(Check {
      name: name.into(),
      ok,
      detail: detail.into(),
    });
    ok
  }

  /// Records that `actual` equals `expected`.
  pub fn eq<T: PartialEq + Debug>(&mut self, name: impl Into<String>, expected: T, actual: T) -> bool {
    let ok = expected == actual;
    let detail = if ok {
      format!("{actual:?}")
    } else {
      format!("expected {expected:?}, got {actual:?}")
    };
    self.check(name, ok, detail)
  }

  /// Records a value the driving program printed: `key` equals `expected`.
  pub fn value(&mut self, console: &Console, key: &str, expected: &str) -> bool {
    self.eq(format!("console {key}"), Some(expected.to_string()), console.value(key))
  }

  /// Records that the cell does not assert something because the product
  /// does not do it yet, for `reason` (which names the deferral). The cell
  /// still asserts everything else; it is not covered when all of that
  /// held.
  pub fn defer(&mut self, reason: impl Into<String>) {
    let reason = reason.into();
    if !self.deferred.contains(&reason) {
      self.deferred.push(reason);
    }
  }

  pub fn passed(&self) -> bool {
    self.list.iter().all(|c| c.ok)
  }
}

/// What a driving program printed. Its results are lines `E2E KEY=VALUE`,
/// and `E2E stat PATH OWNER MODE` (or `E2E stat PATH absent`) for files.
#[derive(Debug, Clone, Default)]
pub struct Console {
  pub text: String,
}

/// A file as the driving program's `stat` saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stat {
  Absent,
  /// Owner as `USER:GROUP` (or `UID:GID`) and the octal permission bits.
  Present {
    owner: String,
    mode: String,
  },
}

impl Console {
  pub fn new(text: impl Into<String>) -> Self {
    Self { text: text.into() }
  }

  fn lines(&self) -> impl Iterator<Item = &str> {
    // A terminal's lines end in CR LF; a console's may carry a prefix such
    // as a log time before the marker.
    self.text.lines().filter_map(|l| l.trim_end_matches('\r').split_once("E2E ").map(|(_, rest)| rest))
  }

  /// The value of the first `E2E KEY=VALUE` line.
  pub fn value(&self, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    self.lines().find_map(|l| l.strip_prefix(&prefix).map(|v| v.trim_end().to_string()))
  }

  /// How the program's `stat` (tag `stat`, or `xstat` from an executor)
  /// saw `path`.
  pub fn stat(&self, tag: &str, path: &str) -> Option<Stat> {
    let prefix = format!("{tag} {path} ");
    self.lines().find_map(|l| {
      let rest = l.strip_prefix(&prefix)?.trim_end();
      if rest == "absent" {
        return Some(Stat::Absent);
      }
      let (owner, mode) = rest.split_once(' ')?;
      Some(Stat::Present {
        owner: owner.to_string(),
        mode: mode.to_string(),
      })
    })
  }

  pub fn contains(&self, needle: &str) -> bool {
    self.text.contains(needle)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn console_reads_values_and_stats() {
    let console = Console::new(
      "boot noise\r\nE2E got=hello world\r\n  0.1s INFO E2E stat /home/agent/.config agent:agent 755\nE2E stat /x absent\nE2E got=second\n",
    );
    assert_eq!(console.value("got").as_deref(), Some("hello world"));
    assert_eq!(console.value("missing"), None);
    assert_eq!(
      console.stat("stat", "/home/agent/.config"),
      Some(Stat::Present {
        owner: "agent:agent".into(),
        mode: "755".into()
      })
    );
    assert_eq!(console.stat("stat", "/x"), Some(Stat::Absent));
    assert_eq!(console.stat("xstat", "/x"), None);
  }

  #[test]
  fn checks_fail_when_any_fails() {
    let mut checks = Checks::default();
    checks.eq("a", 1, 1);
    assert!(checks.passed());
    checks.eq("b", 1, 2);
    assert!(!checks.passed());
    assert_eq!(checks.list.iter().filter(|c| !c.ok).count(), 1);
  }

  #[test]
  fn a_deferral_is_recorded_once() {
    let mut checks = Checks::default();
    checks.defer("deferred: D1");
    checks.defer("deferred: D1");
    assert_eq!(checks.deferred, ["deferred: D1"]);
    assert!(checks.passed());
  }
}
