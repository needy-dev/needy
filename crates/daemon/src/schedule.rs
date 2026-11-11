//! Scheduling: which runner a `runner:` selector of a profile names.
//!
//! The rule is deterministic and leaves no choice to chance:
//!
//! - `runner: NAME` picks that runner. It must have joined, hold the role
//!   (`actor` or `executor`) and be connected (its control stream open).
//! - `runner: { labels: [...] }` picks the one connected runner that holds
//!   the role and carries every label. None is an error naming every
//!   candidate and why it does not fit; several are an error naming them.
//!
//! So the same profile against the same connected runners always lands on
//! the same machine, and a profile never moves silently when another
//! runner with the same labels connects.
// TODO(runner-schedule): choosing among several runners that fit (by load,
// by name, warm pools) is deferred; ambiguity is an error until the owner
// decides how a run should pick, see the runners-and-runs design's open
// questions.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context as _;
use needy_api_server::control::Placement;
use needy_config::profile::{LabelMatch, Provider, RunnerSelector};
use needy_core::types::RunnerRole;

use crate::Daemon;
use crate::pool::Site;

/// A runner as the scheduler sees it: removed runners are left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
  pub name: String,
  pub roles: BTreeSet<RunnerRole>,
  pub labels: BTreeMap<String, String>,
  /// Its control stream is open, so a task reaches it now.
  pub connected: bool,
}

/// Why a selector names no runner.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Unschedulable {
  #[error("no runner is named {name}; {}", joined(.known))]
  NoSuchRunner { name: String, known: Vec<String> },
  #[error("runner {name} cannot host an {role}: its roles are {}", list_roles(.roles))]
  LacksRole {
    name: String,
    role: RunnerRole,
    roles: BTreeSet<RunnerRole>,
  },
  #[error("runner {0} is not connected")]
  NotConnected(String),
  #[error("no connected runner with the {role} role has labels {}; {}", list_labels(.labels), list_candidates(.candidates))]
  NoMatch {
    role: RunnerRole,
    labels: Vec<LabelMatch>,
    /// Each runner and why it does not fit.
    candidates: Vec<(String, String)>,
  },
  #[error(
    "labels {} fit {} connected runners with the {role} role: {}; name one with `runner: NAME` or add a label that tells them apart",
    list_labels(.labels), .matches.len(), .matches.join(", ")
  )]
  Ambiguous {
    role: RunnerRole,
    labels: Vec<LabelMatch>,
    matches: Vec<String>,
  },
}

fn joined(known: &[String]) -> String {
  if known.is_empty() {
    "no runner has joined".into()
  } else {
    format!("the runners are {}", known.join(", "))
  }
}

fn list_roles(roles: &BTreeSet<RunnerRole>) -> String {
  if roles.is_empty() {
    return "none".into();
  }
  roles.iter().map(|r| r.as_str()).collect::<Vec<_>>().join(", ")
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

/// The runner `selector` names for an instance of `role`, by the rule in the
/// module documentation.
pub fn pick(selector: &RunnerSelector, role: RunnerRole, runners: &[Candidate]) -> Result<String, Unschedulable> {
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
      if !runner.roles.contains(&role) {
        return Err(Unschedulable::LacksRole {
          name: name.clone(),
          role,
          roles: runner.roles.clone(),
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
        match misfit(runner, role, wanted) {
          None => matches.push(runner.name.clone()),
          Some(why) => misfits.push((runner.name.clone(), why)),
        }
      }
      match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => Err(Unschedulable::NoMatch {
          role,
          labels: wanted.clone(),
          candidates: misfits,
        }),
        _ => Err(Unschedulable::Ambiguous {
          role,
          labels: wanted.clone(),
          matches,
        }),
      }
    }
  }
}

/// Why `runner` does not fit a label selector for `role`, or `None` when it
/// does.
fn misfit(runner: &Candidate, role: RunnerRole, wanted: &[LabelMatch]) -> Option<String> {
  if !runner.roles.contains(&role) {
    return Some(format!("no {role} role"));
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

/// The host a `runner:` selector names for an instance of `role`: the
/// control host itself when there is no selector.
pub(crate) fn placement(selector: Option<&RunnerSelector>, role: RunnerRole, runners: &[Candidate]) -> Result<Placement, Unschedulable> {
  match selector {
    None => Ok(Placement::ControlHost),
    Some(selector) => pick(selector, role, runners).map(Placement::Runner),
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
          roles: runner.roles,
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
         (`needy daemon --mode control --local-runner NAME`)"
      )),
    }
  }

  /// Where the pool creates an executor whose provider is `provider` at
  /// `placement`.
  pub(crate) fn site(&self, placement: &Placement, provider: Provider) -> Result<Site, String> {
    match (placement, provider) {
      (Placement::ControlHost, _) => Ok(Site::ControlHost),
      // The control plane creates the containers of its own runner.
      (Placement::Runner(name), Provider::Docker) if self.is_local(name) => Ok(Site::ControlHost),
      // TODO(runner-docker): a runner that creates containers itself; see
      // `pool::Pool::create`.
      (Placement::Runner(name), Provider::Docker) => Err(format!(
        "runner {name} cannot create Docker executors yet: only the control host's own runner can \
         (`needy daemon --mode control --local-runner NAME`); use `provider: native` there"
      )),
      (Placement::Runner(name), _) => Ok(Site::Runner {
        name: name.clone(),
        os: self.runner_host(name).map(|host| host.os).unwrap_or_default(),
      }),
    }
  }
}

#[cfg(test)]
#[path = "schedule_test.rs"]
mod tests;
