//! Fault injection. A fault fires from the profile's seed, its index and how
//! many times it has matched, so a replayed run injects the same faults.
//!
//! [`Injector`] is the second `exec.decide` interceptor and the second
//! `net.request` and `net.connect` interceptor; the SFTP tap ([`sftp`]) asks
//! it directly for `file.open`.

pub mod sftp;

use std::sync::Mutex;

use glob_match::glob_match;
use needy_common::seed::mix;
use needy_config::profile::{Chaos, Fault, FaultKind};

use crate::intercept::{ExecContext, ExecDecide, NetContext, NetDecide};
use crate::pattern::Pattern;
use crate::types::{Decision, Effect, Fired, NetDecision, NetOutcome, Outcome};

/// What a fault is checked against.
#[derive(Debug, Clone, Copy)]
pub enum Subject<'a> {
  /// A command as typed.
  Exec(&'a [String]),
  /// A host at the gateway.
  NetConnect(&'a str),
  /// A path relative to an actor mount.
  FileOpen(&'a str),
}

impl Subject<'_> {
  fn kind(&self) -> FaultKind {
    match self {
      Self::Exec(_) => FaultKind::Exec,
      Self::NetConnect(_) => FaultKind::NetConnect,
      Self::FileOpen(_) => FaultKind::FileOpen,
    }
  }

  fn target(&self) -> String {
    match self {
      Self::Exec(argv) => argv.join(" "),
      Self::NetConnect(text) | Self::FileOpen(text) => text.to_string(),
    }
  }
}

/// A profile's faults with their patterns compiled, and how often each has
/// matched.
pub struct Injector {
  seed: u64,
  faults: Vec<Fault>,
  /// The command pattern of each `exec` fault that has one.
  commands: Vec<Option<Pattern>>,
  counts: Mutex<Vec<u64>>,
}

impl Injector {
  pub fn new(chaos: &Chaos) -> Self {
    let commands = chaos
      .faults
      .iter()
      .map(|fault| match (&fault.matcher, fault.on) {
        (Some(pattern), FaultKind::Exec) => Some(Pattern::compile(pattern)),
        _ => None,
      })
      .collect();
    Self {
      seed: chaos.seed,
      faults: chaos.faults.clone(),
      commands,
      counts: Mutex::new(vec![0; chaos.faults.len()]),
    }
  }

  pub fn has(&self, kind: FaultKind) -> bool {
    self.faults.iter().any(|f| f.on == kind)
  }

  pub fn faults(&self) -> &[Fault] {
    &self.faults
  }

  /// The faults whose pattern matches `subject`, whether or not they would
  /// fire; nothing is counted.
  pub fn matching(&self, subject: Subject<'_>) -> Vec<&Fault> {
    (0..self.faults.len()).filter(|&index| self.matches(index, subject)).map(|index| &self.faults[index]).collect()
  }

  /// Counts a matching occurrence and reports the first fault that fires.
  pub fn check(&self, subject: Subject<'_>) -> Option<Fired> {
    // NOTICE: a poisoned lock only means another check panicked mid-count;
    // the counts are still usable.
    let mut counts = self.counts.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    for (index, fault) in self.faults.iter().enumerate() {
      if !self.matches(index, subject) {
        continue;
      }
      counts[index] += 1;
      let n = counts[index];
      let fires = match (fault.nth, fault.probability) {
        (Some(nth), _) => n == nth,
        (None, Some(p)) => {
          let roll = mix(self.seed, (index as u64) << 32 | n) as f64 / u64::MAX as f64;
          roll < p
        }
        (None, None) => true,
      };
      if fires {
        return Some(Fired {
          kind: fault.on,
          target: subject.target(),
          effect: Effect {
            errno: fault.errno.clone(),
            delay: fault.delay.map(Into::into),
            temp_mc: None,
          },
        });
      }
    }
    None
  }

  fn matches(&self, index: usize, subject: Subject<'_>) -> bool {
    let fault = &self.faults[index];
    if fault.on != subject.kind() {
      return false;
    }
    match (subject, &fault.matcher) {
      (_, None) => true,
      (Subject::Exec(argv), Some(_)) => self.commands[index].as_ref().is_some_and(|pattern| pattern.matches(argv).is_some()),
      (Subject::NetConnect(text) | Subject::FileOpen(text), Some(pattern)) => {
        glob_match(pattern, text) || glob_match(pattern, text.trim_start_matches('/'))
      }
    }
  }
}

/// Chaos follows the rules in the `exec.decide` chain. It checks the command
/// as typed unless the command is already denied; an errno fails it, and a
/// delay is left in [`Decision::fired`] for the flow to apply.
impl ExecDecide for Injector {
  fn decide(&self, ctx: &ExecContext, mut decision: Decision) -> Decision {
    if !matches!(decision.outcome, Outcome::Run(_)) {
      return decision;
    }
    if let Some(fired) = self.check(Subject::Exec(&ctx.argv)) {
      if let Some(errno) = &fired.effect.errno {
        decision.outcome = Outcome::Fault(errno.clone());
      }
      decision.fired.push(fired);
    }
    decision
  }
}

/// Chaos follows the allowlist at a gateway. It checks the host unless the
/// request is already refused, so a denied host is not counted; an errno
/// fails the request, and a delay is left in [`NetDecision::fired`] for the
/// gateway to apply.
impl NetDecide for Injector {
  fn decide(&self, ctx: &NetContext, mut decision: NetDecision) -> NetDecision {
    if decision.outcome != NetOutcome::Allow {
      return decision;
    }
    if let Some(fired) = self.check(Subject::NetConnect(&ctx.host)) {
      if let Some(errno) = &fired.effect.errno {
        decision.outcome = NetOutcome::Fault(errno.clone());
      }
      decision.fired.push(fired);
    }
    decision
  }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
