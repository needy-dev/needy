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
use crate::run::{GatewaySite, Run};

/// Refuses to change the network faults of `run` where this daemon's
/// injector is not the one its actor's gateway asks: a remote actor whose
/// runner runs the gateway (S1), or whose runner has not attached yet, so
/// where its gateway runs is not settled.
// TODO(runner-gateway-chaos): pushing a net fault added or removed while the
// run runs to the runner's gateway needs a new `BridgeResponse` case, as
// `Guest::control` streams exec route changes (`Run::update_routes`); not
// part of S1 (2026-11-26-runner-gateway-design.md, "Deferred"). Reopen when
// the owner asks for live network chaos on remote actors.
fn net_faults_apply_here(run: &Run) -> Result<(), ControlError> {
  if !run.is_remote_actor() {
    return Ok(());
  }
  match run.remote_gateway() {
    Some(GatewaySite::Control) => Ok(()),
    Some(GatewaySite::Runner) => Err(ControlError::Failed(format!(
      "run {}'s actor gateway runs on its actor runner, which takes network faults only from the profile's chaos.faults; \
       a network fault cannot be added or removed while the run runs",
      run.id
    ))),
    None => Err(ControlError::Failed(format!(
      "run {}'s actor runner has not attached yet, so where its gateway runs is not settled; try again once the actor started",
      run.id
    ))),
  }
}

impl Daemon {
  pub(crate) async fn add_fault(&self, run: &str, name: &str, fault_yaml: &str, by: Principal) -> Result<FaultAdded, ControlError> {
    let run = self.runs.find_open(run).await?;
    if !valid_fault_name(name) {
      return Err(ControlError::Invalid(format!(
        "fault name {name:?}: a fault's name is a word of letters, digits, `-`, `_` and `.`, at most 64"
      )));
    }
    // The same schema and checks as a profile's `chaos.faults` entry.
    let mut fault: Fault = serde_yaml::from_str(fault_yaml).map_err(|e| ControlError::Invalid(format!("fault {name}: {e}")))?;
    fault.expand_home(&run.profile.user);
    if fault.domain() == FaultDomain::Net {
      net_faults_apply_here(&run)?;
    }
    if fault.domain() == FaultDomain::Sensor {
      // TODO(chaos-sensor): see `actor::schedule_sensor_faults`.
      return Err(ControlError::Invalid("sensor faults are scheduled when the actor starts; add them to the profile's chaos.faults".into()));
    }
    run
      .injector
      .add(name, fault.clone())
      .map_err(|AddError::Exists(_)| ControlError::Invalid(format!("run {} already has a fault named {name:?}; remove it first", run.id)))?;
    // A command the actor did not route until now is routed from here on.
    run.update_routes();
    run.emit(&Event::ChaosAdd(ChaosAdd {
      name: name.to_string(),
      fault: Box::new(fault),
      by: Some(by),
    }));
    Ok(FaultAdded {
      run: run.id.clone(),
      warnings: Vec::new(),
    })
  }

  pub(crate) async fn faults(&self, run: &str) -> Result<RunFaults, ControlError> {
    let run = self.runs.find_open(run).await?;
    // TODO(runner-gateway-chaos): where the actor's runner runs its gateway
    // (`GatewaySite::Runner`), its network faults match and fire there, so
    // their `matched` and `fired` here stay at what they were when the run
    // was placed; the `fault.injected` events in the trace are current.
    // Reporting the runner's counts needs the bridge to carry them.
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
    if run.injector.faults(FaultDomain::Net).iter().any(|(net, _)| net == name) {
      net_faults_apply_here(&run)?;
    }
    if run.injector.remove(name).is_none() {
      return Err(ControlError::NotFound(format!("run {} has no fault named {name:?}", run.id)));
    }
    run.update_routes();
    run.emit(&Event::ChaosRemove(ChaosRemove {
      name: name.to_string(),
      by: Some(by),
    }));
    Ok(run.id.clone())
  }
}

#[cfg(test)]
#[path = "chaos_test.rs"]
mod tests;
