//! Executors: where routed commands run, and how their instances are pooled.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::runner::RunnerSelector;
use crate::value::{Duration, Template};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Executor {
  /// The runner the executor runs on; the control host itself when absent.
  pub runner: Option<RunnerSelector>,
  /// What the runner creates for it: its own process space (`native`, the
  /// default) or a container (`docker`).
  pub provider: Provider,
  pub docker: Option<Docker>,
  /// A recipe reference, for providers that build a machine.
  pub recipe: Option<String>,
  /// The working directory of a command that names none: a path on the
  /// executor's machine.
  pub workspace: Option<String>,
  /// Actor path prefix to executor path prefix.
  pub path_map: BTreeMap<String, String>,
  /// Commands whose key renders equal share one instance: `{run}`,
  /// `{profile}`, `{group}`, `{rule}`.
  pub key: Template,
  /// Matched commands running at once per instance.
  pub max_procs: Option<u32>,
  pub on_full: OnFull,
  pub max_instances: Option<u32>,
  pub idle_timeout: Option<Duration>,
}

impl Default for Executor {
  fn default() -> Self {
    Self {
      runner: None,
      provider: Provider::Native,
      docker: None,
      recipe: None,
      workspace: None,
      path_map: BTreeMap::new(),
      key: Template::new("{run}"),
      max_procs: None,
      on_full: OnFull::Wait,
      max_instances: None,
      idle_timeout: None,
    }
  }
}

/// What creates an executor's instance on its runner.
// NOTICE: `remote` (another machine's `needy daemon --mode executor`) was
// removed with slice 5 of the runners-and-runs design: runners dial the
// control plane and profiles select them with `runner:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
  /// The runner's own process space.
  Native,
  /// A container the runner's machine creates.
  Docker,
  Containerd,
  Firecracker,
  K8s,
}

/// What a command does when its instance has no free slot.
// NOTICE: `new` (start another instance with the same key) was removed
// (owner decision, 2026-11-11): it waited like `wait`, since a second
// instance per key needs `max_instances`, which is not built. A profile that
// still names it is refused by serde's message naming `wait` and `reject`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnFull {
  #[default]
  Wait,
  Reject,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Docker {
  pub image: Option<String>,
  pub build: Option<DockerBuild>,
  /// `docker run` options.
  pub args: Vec<String>,
}

/// `docker build` options.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DockerBuild {
  pub context: String,
  pub dockerfile: Option<String>,
  pub target: Option<String>,
  /// Build arguments.
  pub args: BTreeMap<String, String>,
}
