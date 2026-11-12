//! Cross-references inside a loaded profile: presets, executors and file
//! mappings that rules and settings name. Loading checks each value's form;
//! this checks that the names agree.

use std::fmt;

use crate::profile::{Actions, ActorDriver, ContainerActor, ExecutorDriver, Profile, Side, UnknownPolicy};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
  Error,
  Warning,
}

impl fmt::Display for Severity {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(match self {
      Self::Error => "error",
      Self::Warning => "warning",
    })
  }
}

/// Something a check found in a profile or recipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
  pub severity: Severity,
  pub message: String,
}

impl Finding {
  pub fn error(message: impl Into<String>) -> Self {
    Self {
      severity: Severity::Error,
      message: message.into(),
    }
  }

  pub fn warning(message: impl Into<String>) -> Self {
    Self {
      severity: Severity::Warning,
      message: message.into(),
    }
  }
}

impl fmt::Display for Finding {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}: {}", self.severity, self.message)
  }
}

/// Checks the names a profile uses against the presets, executors and file
/// mappings it defines.
pub fn profile(profile: &Profile) -> Vec<Finding> {
  let mut findings = Vec::new();
  match (profile.actor.driver, &profile.actor.container) {
    (ActorDriver::LinuxContainer, None | Some(ContainerActor { oci_runtime: None, .. })) => findings
      .push(Finding::error("actor.container: a linux-container actor needs `container: { oci_runtime: runc }` (or `runsc`, gVisor)")),
    (ActorDriver::MacosAppleContainer, Some(ContainerActor { oci_runtime: Some(_), .. })) => findings.push(Finding::error(
      "actor.container.oci_runtime is for `driver: linux-container`; Apple's `container` runs each actor in its own VM and has no OCI runtime to choose",
    )),
    (driver @ (ActorDriver::LinuxFirecracker | ActorDriver::MacosVz), Some(_)) => findings.push(Finding::error(format!(
      "actor.container is for `driver: linux-container` or `macos-apple-container`; this actor's driver is {driver}"
    ))),
    _ => {}
  }
  let executor = |name: &str| profile.executors.contains_key(name);
  if let Some(name) = &profile.default_executor
    && !executor(name)
  {
    findings.push(Finding::error(format!("default_executor names unknown executor {name:?}")));
  }
  if let UnknownPolicy::Executor(name) = &profile.unknown
    && !executor(name)
  {
    findings.push(Finding::error(format!("unknown names unknown executor {name:?}")));
  }
  for (name, executor) in &profile.executors {
    if executor.container.is_some() && executor.driver != ExecutorDriver::LinuxContainer {
      findings.push(Finding::error(format!(
        "executors.{name} has `container:` options but its driver is {}; add `driver: linux-container`",
        executor.driver
      )));
    }
  }
  for (name, service) in &profile.actor.network.services {
    if let Some(problem) = service_problem(name, &service.url) {
      findings.push(Finding::error(format!("actor.network.services.{name}: {problem}")));
    }
  }
  for name in profile.chaos.faults.keys() {
    if !crate::profile::valid_fault_name(name) {
      findings
        .push(Finding::error(format!("chaos.faults.{name}: a fault's name is a word of letters, digits, `-`, `_` and `.`, at most 64")));
    }
  }
  for name in &profile.run_files {
    if !profile.files.mappings.contains_key(name) {
      findings.push(Finding::error(format!("run_files names unknown file mapping {name:?}")));
    }
  }
  for (name, preset) in &profile.presets {
    check_actions(profile, &format!("presets.{name}"), preset, &mut findings);
  }
  for (index, rule) in profile.commands.iter().enumerate() {
    check_actions(profile, &format!("commands[{index}] ({})", rule.matcher), &rule.actions, &mut findings);
  }
  for (name, mapping) in &profile.files.mappings {
    for (end, side) in [("from", &mapping.from), ("to", &mapping.to)] {
      if let Side::Executor { name: exec, .. } = side
        && !executor(exec)
      {
        findings.push(Finding::error(format!("files.{name}.{end} names unknown executor {exec:?}")));
      }
    }
  }
  findings
}

/// What is wrong with a service named `name` that forwards to `url`, if
/// anything: its name becomes the host `NAME.needy`, so it is a DNS label,
/// and its URL is an absolute `http://` or `https://` URL.
fn service_problem(name: &str, url: &str) -> Option<String> {
  let label = !name.is_empty()
    && name.len() <= 63
    && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    && !name.starts_with('-')
    && !name.ends_with('-');
  if !label {
    return Some("a service name is a DNS label: lowercase letters, digits and `-`, at most 63".into());
  }
  let parsed = match url.parse::<http::Uri>() {
    Ok(parsed) => parsed,
    Err(e) => return Some(format!("url {url:?} is not a URL: {e}")),
  };
  match (parsed.scheme_str(), parsed.authority()) {
    (Some("http" | "https"), Some(_)) => None,
    _ => Some(format!("url {url:?} must be an absolute http:// or https:// URL")),
  }
}

/// The presets, placement and file mappings that a rule or preset names.
fn check_actions(profile: &Profile, at: &str, actions: &Actions, findings: &mut Vec<Finding>) {
  for name in &actions.uses {
    if !profile.presets.contains_key(name) {
      findings.push(Finding::error(format!("{at} uses unknown preset {name:?}")));
    }
  }
  let named = [actions.to.as_deref(), actions.executor.as_deref()];
  for name in named.into_iter().flatten() {
    match name {
      "actor" => {}
      // `to: executor` takes the rule's `executor:`, or the default.
      "executor" if actions.executor.is_some() || profile.default_executor.is_some() => {}
      "executor" => findings.push(Finding::error(format!("{at} sends to `executor`, but there is no default_executor"))),
      name if !profile.executors.contains_key(name) => findings.push(Finding::error(format!("{at} sends to unknown executor {name:?}"))),
      _ => {}
    }
  }
  for name in &actions.files {
    if !profile.files.mappings.contains_key(name) {
      findings.push(Finding::error(format!("{at} adds unknown file mapping {name:?}")));
    }
  }
}

#[cfg(test)]
#[path = "validate_test.rs"]
mod tests;
