//! Fault injection. A fault fires from the seed, its name and how many times
//! it has matched, so a replayed run injects the same faults.
//!
//! [`Injector`] is the second `exec.decide` interceptor and the second
//! `net.request` and `net.connect` interceptor; the SFTP fault filter
//! ([`sftp::FileFaults`]), the `file.open` interceptor, asks it directly.
//! Its faults are the profile's, plus those `needy chaos add` adds while
//! the run runs, less those `needy chaos rm` removes.

pub mod sftp;

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use needy_common::seed::mix;
use needy_config::profile::{Chaos, Fault, FaultDomain};
use serde::{Deserialize, Serialize};

use crate::intercept::exec::{Decision, ExecContext, ExecDecide};
use crate::intercept::net::{NetContext, NetDecide, NetDecision, NetOutcome};
use crate::pattern::Pattern;
use crate::types::{Effect, Fired, Outcome};

/// What a fault is checked against.
#[derive(Debug, Clone, Copy)]
pub enum Subject<'a> {
  /// A command as typed.
  Exec(&'a [String]),
  /// A path the actor opens on a workspace mount, as the actor sees it.
  FileOpen(&'a str),
  /// A host at the gateway: a tunnel or plain-HTTP request to it.
  Net {
    host: &'a str,
    /// A plain-HTTP request, which `net: { request: ... }` faults match
    /// too; `false` for a `CONNECT` tunnel.
    request: bool,
  },
}

impl Subject<'_> {
  fn domain(&self) -> FaultDomain {
    match self {
      Self::Exec(_) => FaultDomain::Exec,
      Self::FileOpen(_) => FaultDomain::File,
      Self::Net { .. } => FaultDomain::Net,
    }
  }

  fn target(&self) -> String {
    match self {
      Self::Exec(argv) => argv.join(" "),
      Self::FileOpen(text) | Self::Net { host: text, .. } => text.to_string(),
    }
  }
}

/// Where a fault of a run came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultSource {
  /// The run's profile.
  Profile,
  /// `needy chaos add`, while the run runs.
  Added,
}

impl FaultSource {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Profile => "profile",
      Self::Added => "added",
    }
  }
}

/// One fault of a run with its counts, as `needy chaos ls` lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct FaultState {
  pub name: String,
  pub fault: Fault,
  pub source: FaultSource,
  /// Operations it matched, within its time window.
  pub matched: u64,
  /// Times it fired.
  pub fired: u64,
}

/// Why a fault cannot be added.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AddError {
  #[error("the run already has a fault named {0:?}; remove it first")]
  Exists(String),
}

struct Entry {
  fault: Fault,
  source: FaultSource,
  /// The command pattern of an exec fault.
  pattern: Option<Pattern>,
  matched: u64,
  fired: u64,
}

impl Entry {
  fn new(fault: Fault, source: FaultSource) -> Self {
    let pattern = fault.exec.as_deref().map(Pattern::compile);
    Self {
      fault,
      source,
      pattern,
      matched: 0,
      fired: 0,
    }
  }

  fn matches(&self, subject: Subject<'_>) -> bool {
    let fault = &self.fault;
    if fault.domain() != subject.domain() {
      return false;
    }
    match subject {
      Subject::Exec(argv) => self.pattern.as_ref().is_some_and(|pattern| pattern.matches(argv).is_some()),
      Subject::FileOpen(path) => fault.file.as_ref().is_some_and(|file| glob_match::glob_match(&file.open, path)),
      Subject::Net { host, request } => match &fault.net {
        Some(net) => match (&net.connect, &net.request) {
          (Some(pattern), _) => pattern.matches_host(host),
          (None, Some(pattern)) => request && pattern.matches_host(host),
          (None, None) => false,
        },
        None => false,
      },
    }
  }
}

/// A run's faults with their patterns compiled and their counts.
pub struct Injector {
  seed: u64,
  /// When the run was created: `after` and `until` count from here.
  created: Instant,
  faults: Mutex<BTreeMap<String, Entry>>,
}

impl Injector {
  /// The faults of a run created now.
  pub fn new(chaos: &Chaos) -> Self {
    Self::since(chaos, Instant::now())
  }

  /// The faults of a run created at `created`.
  pub fn since(chaos: &Chaos, created: Instant) -> Self {
    let faults = chaos.faults.iter().map(|(name, fault)| (name.clone(), Entry::new(fault.clone(), FaultSource::Profile))).collect();
    Self {
      seed: chaos.seed,
      created,
      faults: Mutex::new(faults),
    }
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Entry>> {
    // NOTICE: a poisoned lock only means another check panicked mid-count;
    // the counts are still usable.
    self.faults.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }

  /// Adds a fault while the run runs. Its counts start at zero.
  pub fn add(&self, name: &str, fault: Fault) -> Result<(), AddError> {
    let mut faults = self.lock();
    if faults.contains_key(name) {
      return Err(AddError::Exists(name.to_string()));
    }
    faults.insert(name.to_string(), Entry::new(fault, FaultSource::Added));
    Ok(())
  }

  /// Removes a fault, whether the profile or `needy chaos add` added it;
  /// `None` when the run has none of that name.
  pub fn remove(&self, name: &str) -> Option<Fault> {
    self.lock().remove(name).map(|entry| entry.fault)
  }

  /// The run's faults in name order, with their counts.
  pub fn list(&self) -> Vec<FaultState> {
    self
      .lock()
      .iter()
      .map(|(name, entry)| FaultState {
        name: name.clone(),
        fault: entry.fault.clone(),
        source: entry.source,
        matched: entry.matched,
        fired: entry.fired,
      })
      .collect()
  }

  /// Whether the run has a fault in `domain` now.
  pub fn has(&self, domain: FaultDomain) -> bool {
    self.lock().values().any(|entry| entry.fault.domain() == domain)
  }

  /// The run's faults in `domain`, by name.
  pub fn faults(&self, domain: FaultDomain) -> Vec<(String, Fault)> {
    self.lock().iter().filter(|(_, entry)| entry.fault.domain() == domain).map(|(name, entry)| (name.clone(), entry.fault.clone())).collect()
  }

  /// The faults whose subject matches, whether or not they would fire;
  /// nothing is counted.
  pub fn matching(&self, subject: Subject<'_>) -> Vec<(String, Fault)> {
    self.lock().iter().filter(|(_, entry)| entry.matches(subject)).map(|(name, entry)| (name.clone(), entry.fault.clone())).collect()
  }

  /// Counts a matching occurrence on every fault whose subject and time
  /// window match, and reports the first, in name order, that fires.
  pub fn check(&self, subject: Subject<'_>) -> Option<Fired> {
    let elapsed = self.created.elapsed();
    let mut faults = self.lock();
    let mut fired = None;
    for (name, entry) in faults.iter_mut() {
      if !entry.matches(subject) || !within(&entry.fault, elapsed) {
        continue;
      }
      entry.matched += 1;
      if fired.is_some() || !self.fires(name, entry) {
        continue;
      }
      entry.fired += 1;
      fired = Some(Fired {
        name: name.clone(),
        domain: subject.domain(),
        target: subject.target(),
        effect: effect(&entry.fault),
      });
    }
    fired
  }

  /// Whether `entry`, which just matched for the `entry.matched`th time,
  /// fires: at most `times` times, on the `nth` match or with
  /// `probability`, rolled from the seed, the fault's name and the match
  /// count, and otherwise on every match.
  fn fires(&self, name: &str, entry: &Entry) -> bool {
    let fault = &entry.fault;
    if fault.times.is_some_and(|times| entry.fired >= times) {
      return false;
    }
    match (fault.nth, fault.probability) {
      (Some(nth), _) => entry.matched == nth,
      (None, Some(p)) => {
        let roll = mix(self.seed, mix(name_key(name), entry.matched)) as f64 / u64::MAX as f64;
        roll < p
      }
      (None, None) => true,
    }
  }
}

/// Whether `elapsed` since the run was created lies in the fault's time
/// window, `after` included and `until` not.
fn within(fault: &Fault, elapsed: Duration) -> bool {
  fault.after.is_none_or(|after| elapsed >= *after) && fault.until.is_none_or(|until| elapsed < *until)
}

/// A fault's name as a number for the roll, so a fault's rolls do not
/// change when other faults are added or removed.
// NOTICE: FNV-1a, written out rather than `std::hash`, whose `DefaultHasher`
// may change between Rust releases; a replay must roll the same numbers.
// Source: http://www.isthe.com/chongo/tech/comp/fnv/index.html#FNV-param
// (64-bit offset basis 14695981039346656037, prime 1099511628211; no
// versioned page, read 2026-11-12).
fn name_key(name: &str) -> u64 {
  name.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3))
}

/// What a fault does when it fires.
fn effect(fault: &Fault) -> Effect {
  Effect {
    delay: fault.delay.map(Into::into),
    exit: fault.exit,
    signal: fault.signal.clone(),
    stderr: fault.stderr.clone(),
    hang: fault.hang,
    errno: fault.errno.clone(),
    refuse: fault.refuse,
    reset: fault.reset,
    status: fault.status,
    temp_mc: fault.sensor.as_ref().and_then(|readings| readings.temp_c).map(|c| (c * 1000.0) as i32),
  }
}

/// A sensor fault's reading in millidegrees Celsius: its `temp_c`, or 100
/// degrees, a reading past the fake coretemp's critical point.
pub fn sensor_temp_mc(fault: &Fault) -> i32 {
  fault.sensor.as_ref().and_then(|readings| readings.temp_c).map(|c| (c * 1000.0) as i32).unwrap_or(100_000)
}

/// Chaos follows the rules in the `exec.decide` chain. It checks the command
/// as typed unless the command is already denied. A fault that ends the
/// command (exit, signal, hang) becomes the outcome; its effect, and a delay
/// for the flow to wait out first, are in [`Decision::fired`].
impl ExecDecide for Injector {
  fn decide(&self, ctx: &ExecContext, mut decision: Decision) -> Decision {
    if !matches!(decision.outcome, Outcome::Run(_)) {
      return decision;
    }
    if let Some(fired) = self.check(Subject::Exec(&ctx.argv)) {
      if fired.effect.ends() {
        decision.outcome = Outcome::Fault(fired.name.clone());
      }
      decision.fired.push(fired);
    }
    decision
  }
}

/// Chaos follows the allowlist at a gateway. It checks the host unless the
/// request is already refused, so a denied host is not counted. A fault
/// that fails the connection becomes the outcome; its effect, and a delay
/// for the gateway to wait out first, are in [`NetDecision::fired`].
impl NetDecide for Injector {
  fn decide(&self, ctx: &NetContext, mut decision: NetDecision) -> NetDecision {
    if decision.outcome != NetOutcome::Allow {
      return decision;
    }
    let subject = Subject::Net {
      host: &ctx.host,
      request: ctx.path.is_some(),
    };
    if let Some(fired) = self.check(subject) {
      if fired.effect.ends() {
        decision.outcome = NetOutcome::Fault(fired.name.clone());
      }
      decision.fired.push(fired);
    }
    decision
  }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
