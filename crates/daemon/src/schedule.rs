//! Scheduling: which runner a `runner:` selector of a profile names.
//!
//! The rule is deterministic and leaves no choice to chance:
//!
//! - `runner: NAME` picks that runner. It must have joined, have the
//!   capability the instance needs (`actor(DRIVER)` for an actor of that
//!   driver, `executor(DRIVER)` for an executor) and be connected (its control
//!   stream open).
//! - `runner: { labels: [...] }` picks the one connected runner that has the
//!   capability and carries every label. None is an error naming every
//!   candidate and why it does not fit; several are an error naming them.
//!
//! So the same profile against the same connected runners always lands on
//! the same machine, and a profile never moves silently when another
//! runner with the same labels connects.
// TODO(runner-schedule): choosing among several runners that fit (by load,
// by name, warm pools) is deferred; ambiguity is an error until the owner
// decides how a run should pick, see the runners-and-runs design's open
// questions.

use std::collections::BTreeMap;
use std::fmt;

use anyhow::Context as _;
use needy_api_server::control::Placement;
use needy_config::profile::{ActorDriver, ExecutorDriver, LabelMatch, RunnerSelector};
use needy_core::types::{Capabilities, RunnerRole};

use crate::Daemon;
use crate::pool::Site;

/// A runner as the scheduler sees it: removed runners are left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
  pub name: String,
  /// As it last reported them.
  pub capabilities: Capabilities,
  pub labels: BTreeMap<String, String>,
  /// Its control stream is open, so a task reaches it now.
  pub connected: bool,
}

/// What an instance needs of the runner it is placed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
  /// An actor of this driver.
  Actor(ActorDriver),
  /// An executor of this driver.
  Executor(ExecutorDriver),
}

impl Need {
  fn role(self) -> RunnerRole {
    match self {
      Self::Actor(_) => RunnerRole::Actor,
      Self::Executor(_) => RunnerRole::Executor,
    }
  }

  /// Whether a runner with `capabilities` can host the instance.
  pub fn met_by(self, capabilities: &Capabilities) -> bool {
    match self {
      Self::Actor(driver) => capabilities.actor.contains(&driver),
      Self::Executor(driver) => capabilities.executor.contains(&driver),
    }
  }
}

impl fmt::Display for Need {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Actor(driver) => write!(f, "actor({driver})"),
      Self::Executor(driver) => write!(f, "executor({driver})"),
    }
  }
}

/// Why a selector names no runner.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Unschedulable {
  #[error("no runner is named {name}; {}", joined(.known))]
  NoSuchRunner { name: String, known: Vec<String> },
  #[error("runner {name} cannot host {need}: it has {capabilities}; {}", list_able(*.need, .able))]
  Lacks {
    name: String,
    need: Need,
    capabilities: Capabilities,
    /// The runners that have it.
    able: Vec<String>,
  },
  #[error("runner {0} is not connected")]
  NotConnected(String),
  #[error("no connected runner with {need} has labels {}; {}", list_labels(.labels), list_candidates(.candidates))]
  NoMatch {
    need: Need,
    labels: Vec<LabelMatch>,
    /// Each runner and why it does not fit.
    candidates: Vec<(String, String)>,
  },
  #[error(
    "labels {} fit {} connected runners with {need}: {}; name one with `runner: NAME` or add a label that tells them apart",
    list_labels(.labels), .matches.len(), .matches.join(", ")
  )]
  Ambiguous {
    need: Need,
    labels: Vec<LabelMatch>,
    matches: Vec<String>,
  },
}

impl Unschedulable {
  /// Facts come from the scheduler's actual candidates, never message parsing.
  pub(crate) fn diagnostic(&self) -> needy_api_server::control::Diagnostic {
    let mut facts = BTreeMap::new();
    let reason = match self {
      Self::NoSuchRunner { name, known } => {
        facts.insert("runner".into(), name.clone());
        facts.insert("candidates".into(), known.join(", "));
        "runner.not_found"
      }
      Self::Lacks {
        name, need, able, ..
      } => {
        facts.insert("runner".into(), name.clone());
        facts.insert("need".into(), need.to_string());
        facts.insert("candidates".into(), able.join(", "));
        "runner.capability_missing"
      }
      Self::NotConnected(name) => {
        facts.insert("runner".into(), name.clone());
        "runner.not_connected"
      }
      Self::NoMatch {
        need,
        labels,
        candidates,
      } => {
        facts.insert("need".into(), need.to_string());
        facts.insert("selector".into(), list_labels(labels));
        facts.insert("candidates".into(), candidates.iter().map(|(name, why)| format!("{name} ({why})")).collect::<Vec<_>>().join(", "));
        "runner.selector_no_match"
      }
      Self::Ambiguous {
        need,
        labels,
        matches,
      } => {
        facts.insert("need".into(), need.to_string());
        facts.insert("selector".into(), list_labels(labels));
        facts.insert("candidates".into(), matches.join(", "));
        "runner.selector_ambiguous"
      }
    };
    needy_api_server::control::Diagnostic {
      reason: reason.into(),
      facts,
    }
  }
}

fn joined(known: &[String]) -> String {
  if known.is_empty() {
    "no runner has joined".into()
  } else {
    format!("the runners are {}", known.join(", "))
  }
}

fn list_able(need: Need, able: &[String]) -> String {
  if able.is_empty() {
    format!("no runner has {need}")
  } else {
    format!("runners with {need}: {}", able.join(", "))
  }
}

fn list_labels(labels: &[LabelMatch]) -> String {
  labels.iter().map(LabelMatch::to_string).collect::<Vec<_>>().join(",")
}

fn list_candidates(candidates: &[(String, String)]) -> String {
  if candidates.is_empty() {
    return "no runner has joined".into();
  }
  let listed: Vec<String> = candidates.iter().map(|(name, why)| format!("{name} ({why})")).collect();
  format!("candidates: {}", listed.join(", "))
}

/// The runner `selector` names for an instance that needs `need`, by the
/// rule in the module documentation.
pub fn pick(selector: &RunnerSelector, need: Need, runners: &[Candidate]) -> Result<String, Unschedulable> {
  let mut runners: Vec<&Candidate> = runners.iter().collect();
  runners.sort_by(|a, b| a.name.cmp(&b.name));
  match selector {
    RunnerSelector::Name(name) => {
      let Some(runner) = runners.iter().find(|r| &r.name == name) else {
        return Err(Unschedulable::NoSuchRunner {
          name: name.clone(),
          known: runners.iter().map(|r| r.name.clone()).collect(),
        });
      };
      if !need.met_by(&runner.capabilities) {
        return Err(Unschedulable::Lacks {
          name: name.clone(),
          need,
          capabilities: runner.capabilities.clone(),
          able: runners.iter().filter(|r| need.met_by(&r.capabilities)).map(|r| r.name.clone()).collect(),
        });
      }
      if !runner.connected {
        return Err(Unschedulable::NotConnected(name.clone()));
      }
      Ok(name.clone())
    }
    RunnerSelector::Labels(wanted) => {
      let mut matches = Vec::new();
      let mut misfits = Vec::new();
      for runner in runners {
        match misfit(runner, need, wanted) {
          None => matches.push(runner.name.clone()),
          Some(why) => misfits.push((runner.name.clone(), why)),
        }
      }
      match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => Err(Unschedulable::NoMatch {
          need,
          labels: wanted.clone(),
          candidates: misfits,
        }),
        // TODO(runner-schedule): a load-balancing policy plugs in here: it
        // would choose one of `matches` (sorted by name) instead of refusing,
        // and need each runner's load in `Candidate`. Ambiguity stays an
        // error until the owner picks a policy and decides how a replay lands
        // on the same runner (owner decision, 2026-11-11).
        _ => Err(Unschedulable::Ambiguous {
          need,
          labels: wanted.clone(),
          matches,
        }),
      }
    }
  }
}

/// Why `runner` does not fit a label selector for `need`, or `None` when it
/// does.
fn misfit(runner: &Candidate, need: Need, wanted: &[LabelMatch]) -> Option<String> {
  if !runner.capabilities.roles().contains(&need.role()) {
    return Some(format!("no {} role", need.role()));
  }
  if !need.met_by(&runner.capabilities) {
    return Some(format!("has {}", runner.capabilities));
  }
  if let Some(missing) = wanted.iter().find(|label| !label.matches(&runner.labels)) {
    return Some(format!("no label {missing}"));
  }
  if !runner.connected {
    return Some("not connected".into());
  }
  None
}

// ------------------------------------------------------------- placement

/// The host a `runner:` selector names for an instance that needs `need`:
/// the control host itself when there is no selector.
pub(crate) fn placement(selector: Option<&RunnerSelector>, need: Need, runners: &[Candidate]) -> Result<Placement, Unschedulable> {
  match selector {
    None => Ok(Placement::ControlHost),
    Some(selector) => pick(selector, need, runners).map(Placement::Runner),
  }
}

impl Daemon {
  /// Every runner that was not removed, and whether it is connected now.
  pub(crate) async fn candidates(&self) -> anyhow::Result<Vec<Candidate>> {
    let runners = self.state.runners().await.context("cannot read the runner registry")?;
    Ok(
      runners
        .into_iter()
        .filter(|runner| runner.removed_ms.is_none())
        .map(|runner| Candidate {
          connected: self.links.get(&runner.name).is_some(),
          name: runner.name,
          capabilities: runner.capabilities,
          labels: runner.labels,
        })
        .collect(),
    )
  }

  /// Whether `runner` runs on this host, as the operator said.
  pub(crate) fn is_local(&self, runner: &str) -> bool {
    self.config.local_runner.as_deref() == Some(runner)
  }

  /// Why an actor cannot run at `placement`, if it cannot: an actor runs on
  /// the control host, reached directly or as its own runner.
  // TODO(runner-actor): an actor on another runner needs the actor role
  // over dial-out (the VM's lifecycle and its guest channel relayed through
  // the runner); see `Config::local_runner`.
  pub(crate) fn actor_refusal(&self, placement: &Placement) -> Option<String> {
    match placement {
      Placement::ControlHost => None,
      Placement::Runner(name) if self.is_local(name) => None,
      Placement::Runner(name) => Some(format!(
        "runner {name} cannot host actors yet: an actor runs on the control host, or on its own runner \
         (`needy daemon serve --local-runner NAME`)"
      )),
    }
  }

  /// Where the pool creates an executor whose driver is `driver` at
  /// `placement`.
  pub(crate) fn site(&self, placement: &Placement, driver: ExecutorDriver) -> Result<Site, String> {
    match (placement, driver) {
      (Placement::ControlHost, _) => Ok(Site::ControlHost),
      // The control plane creates the containers of its own runner.
      (Placement::Runner(name), ExecutorDriver::LinuxContainer) if self.is_local(name) => Ok(Site::ControlHost),
      // TODO(runner-docker): a runner that creates containers itself; see
      // `pool::Pool::create`.
      (Placement::Runner(name), ExecutorDriver::LinuxContainer) => Err(format!(
        "runner {name} cannot create linux-container executors yet: only the control host's own runner can \
         (`needy daemon serve --local-runner NAME`); use `driver: native` there"
      )),
      (Placement::Runner(name), _) => {
        let host = self.runner_host(name).unwrap_or_default();
        Ok(Site::Runner {
          name: name.clone(),
          os: host.os,
          home: host.home,
        })
      }
    }
  }
}

#[cfg(test)]
#[path = "schedule_test.rs"]
mod tests;
