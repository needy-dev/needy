//! Cross-references inside a loaded profile: presets, executors and file
//! mappings that rules and settings name. Loading checks each value's form;
//! this checks that the names agree.

use std::fmt;

use crate::profile::{Actions, Profile, Side, UnknownPolicy};

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
  for name in &profile.session_files {
    if !profile.files.mappings.contains_key(name) {
      findings.push(Finding::error(format!("session_files names unknown file mapping {name:?}")));
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
