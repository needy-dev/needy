//! A live run's chaos, through `api-server`'s `Runs` trait: `needy chaos
//! add`, `ls` and `rm`. The run's injector holds its faults; each addition
//! and removal is a trace event (`chaos.add`, `chaos.remove`) at its place
//! in the run's event sequence, so a replay applies it there.

use needy_api_server::control::{ControlError, FaultAdded, FaultInfo, RunFaults};
use needy_config::profile::{Fault, FaultDomain, valid_fault_name};
use needy_core::chaos::AddError;
use needy_core::event::{ChaosAdd, ChaosRemove, Event};
use needy_core::types::Principal;

use crate::Daemon;

impl Daemon {
  pub(crate) async fn add_fault(&self, run: &str, name: &str, fault_yaml: &str, by: Principal) -> Result<FaultAdded, ControlError> {
    let run = self.runs.find_open(run).await?;
    if !valid_fault_name(name) {
      return Err(ControlError::Invalid(format!(
        "fault name {name:?}: a fault's name is a word of letters, digits, `-`, `_` and `.`, at most 64"
      )));
    }
    // The same schema and checks as a profile's `chaos.faults` entry.
    let fault: Fault = serde_yaml::from_str(fault_yaml).map_err(|e| ControlError::Invalid(format!("fault {name}: {e}")))?;
    if fault.domain() == FaultDomain::Sensor {
      // TODO(chaos-sensor): see `actor::schedule_sensor_faults`.
      return Err(ControlError::Invalid("sensor faults are scheduled when the actor starts; add them to the profile's chaos.faults".into()));
    }
    let warnings = unrouted(&run.profile, &fault);
    run
      .injector
      .add(name, fault.clone())
      .map_err(|AddError::Exists(_)| ControlError::Invalid(format!("run {} already has a fault named {name:?}; remove it first", run.id)))?;
    run.emit(&Event::ChaosAdd(ChaosAdd {
      name: name.to_string(),
      fault: Box::new(fault),
      by: Some(by),
    }));
    Ok(FaultAdded {
      run: run.id.clone(),
      warnings,
    })
  }

  pub(crate) async fn faults(&self, run: &str) -> Result<RunFaults, ControlError> {
    let run = self.runs.find_open(run).await?;
    let faults = run
      .injector
      .list()
      .into_iter()
      .map(|state| FaultInfo {
        domain: state.fault.domain().to_string(),
        // NOTICE: a `Fault` is plain fields and always serializes.
        fault_yaml: serde_yaml::to_string(&state.fault).unwrap_or_default(),
        name: state.name,
        source: state.source,
        matched: state.matched,
        fired: state.fired,
      })
      .collect();
    Ok(RunFaults {
      run: run.id.clone(),
      faults,
    })
  }

  pub(crate) async fn remove_fault(&self, run: &str, name: &str, by: Principal) -> Result<String, ControlError> {
    let run = self.runs.find_open(run).await?;
    if run.injector.remove(name).is_none() {
      return Err(ControlError::NotFound(format!("run {} has no fault named {name:?}", run.id)));
    }
    run.emit(&Event::ChaosRemove(ChaosRemove {
      name: name.to_string(),
      by: Some(by),
    }));
    Ok(run.id.clone())
  }
}

/// Why an exec fault added to a running run may not reach some commands:
/// the actor read its route list when it booted, so a command whose name is
/// not on it runs in the actor without asking the daemon.
fn unrouted(profile: &needy_config::profile::Profile, fault: &Fault) -> Vec<String> {
  let Some(pattern) = fault.exec.as_deref() else {
    return Vec::new();
  };
  // Every exec is routed: the fault reaches every command.
  let Some(routed) = needy_core::rules::route_names(profile) else {
    return Vec::new();
  };
  let missing: Vec<String> = match needy_core::pattern::Pattern::compile(pattern).command_names() {
    Some(names) => names.into_iter().filter(|name| !routed.contains(name)).collect(),
    None => vec!["any command".into()],
  };
  if missing.is_empty() {
    return Vec::new();
  }
  vec![format!(
    "the actor routes only the commands its profile named when it booted ({}); in the actor this fault does not reach {}, \
     only `needy exec` commands",
    if routed.is_empty() {
      "none".to_string()
    } else {
      routed.join(", ")
    },
    missing.join(", ")
  )]
}

#[cfg(test)]
#[path = "chaos_test.rs"]
mod tests;
