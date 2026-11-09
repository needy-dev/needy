//! Executors: where routed commands run, and how their instances are pooled.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::value::{Duration, Template};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Executor {
  pub runtime: Runtime,
  pub docker: Option<Docker>,
  /// A recipe reference, for runtimes that build a machine.
  pub recipe: Option<String>,
  pub remote: Option<Remote>,
  pub workspace: Option<String>,
  /// Actor path prefix to executor path prefix.
  pub path_map: BTreeMap<String, String>,
  /// Commands whose key renders equal share one instance: `{session}`,
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
      runtime: Runtime::Docker,
      docker: None,
      recipe: None,
      remote: None,
      workspace: None,
      path_map: BTreeMap::new(),
      key: Template::new("{session}"),
      max_procs: None,
      on_full: OnFull::Wait,
      max_instances: None,
      idle_timeout: None,
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
  Native,
  Docker,
  Containerd,
  Firecracker,
  K8s,
  Remote,
}

/// What a command does when its instance has no free slot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnFull {
  #[default]
  Wait,
  New,
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

/// Another machine's needy daemon.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Remote {
  pub url: String,
  /// The environment variable that holds the bearer token.
  pub token_env: Option<String>,
}
