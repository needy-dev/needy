//! Proto <-> domain mappings for `RunService`.

use needy_api_proto::v1;
use needy_core::event::ActorStop;
use needy_core::intercept::exec::ExecContext;

use crate::control::{
  NewRun, Placement, ProfileCheck, ProfileFinding, ProfileToCheck, Route, RunFaults, RunInfo, RunWorkspace, StopMode, StoredEvent, Target,
};

use super::images::{image_commit, pinned_features};
use super::{Error, proto_actor_state, proto_diagnostic, proto_exit, proto_run_state};

impl From<StoredEvent> for v1::StoredEvent {
  fn from(event: StoredEvent) -> Self {
    Self {
      seq: event.seq,
      time_ms: event.time_ms,
      kind: event.kind,
      version: event.version,
      payload: event.payload,
    }
  }
}

pub fn proto_stored_event(event: StoredEvent) -> v1::StoredEvent {
  event.into()
}

/// How a `StopRunRequest` asks to end the run's actor.
pub fn stop_mode(now: bool, grace_ms: Option<i64>) -> Result<StopMode, Error> {
  match (now, grace_ms) {
    (true, Some(_)) => Err(Error::Contradicts("a stop `now` has no grace period")),
    (true, None) => Ok(StopMode::Now),
    (false, None) => Ok(StopMode::default()),
    (false, Some(ms)) => match u64::try_from(ms) {
      Ok(ms) => Ok(StopMode::Graceful {
        grace: std::time::Duration::from_millis(ms),
      }),
      Err(_) => Err(Error::Contradicts("a grace period cannot be negative")),
    },
  }
}

/// How a stop ended the run's actor, for the caller.
pub fn proto_stopped(stopped: Option<ActorStop>) -> v1::StopRunResponse {
  match stopped {
    None => v1::StopRunResponse::default(),
    Some(ActorStop::Graceful) => v1::StopRunResponse {
      graceful: Some(true),
      reason: String::new(),
    },
    Some(ActorStop::Forced { reason }) => v1::StopRunResponse {
      graceful: Some(false),
      reason,
    },
  }
}

impl TryFrom<v1::CreateRunRequest> for NewRun {
  type Error = Error;

  fn try_from(request: v1::CreateRunRequest) -> Result<Self, Error> {
    if request.wait_for_attach && request.tty.is_none() {
      return Err(Error::Missing("terminal for wait_for_attach"));
    }
    if request.profile_name.is_empty() {
      return Err(Error::Missing("profile"));
    }
    Ok(Self {
      profile_yaml: request.profile_yaml,
      recipe_yaml: request.recipe_yaml,
      profile_name: request.profile_name,
      group: request.group,
      name: request.name,
      actor_args: request.actor_args,
      // The adapter sets who asks; the wire does not say.
      by: None,
      tty: request.tty.map(Into::into),
      wait_for_attach: request.wait_for_attach,
      client_execution: request.client_execution,
      client_dir: request.client_dir,
      // The adapter sets where the client is; the wire does not say.
      on_control_host: false,
      image_commit: request.image_commit.map(image_commit).transpose()?,
      pinned_features: pinned_features(request.pinned_features)?,
    })
  }
}

pub fn new_run(request: v1::CreateRunRequest) -> Result<NewRun, Error> {
  request.try_into()
}

impl From<v1::CheckProfileRequest> for ProfileToCheck {
  fn from(request: v1::CheckProfileRequest) -> Self {
    Self {
      profile_yaml: request.profile_yaml,
      recipe_yaml: request.recipe_yaml,
    }
  }
}

pub fn profile_to_check(request: v1::CheckProfileRequest) -> ProfileToCheck {
  request.into()
}

impl From<RunFaults> for v1::ListFaultsResponse {
  fn from(faults: RunFaults) -> Self {
    use needy_core::chaos::FaultSource;
    Self {
      run_id: faults.run,
      faults: faults
        .faults
        .into_iter()
        .map(|f| v1::FaultInfo {
          name: f.name,
          domain: f.domain,
          fault_yaml: f.fault_yaml,
          source: match f.source {
            FaultSource::Profile => v1::FaultSource::Profile,
            FaultSource::Added => v1::FaultSource::Added,
          }
          .into(),
          matched: f.matched,
          fired: f.fired,
        })
        .collect(),
    }
  }
}

pub fn proto_faults(faults: RunFaults) -> v1::ListFaultsResponse {
  faults.into()
}

impl From<Placement> for v1::Placement {
  fn from(placement: Placement) -> Self {
    Self {
      runner: match placement {
        Placement::ControlHost => String::new(),
        Placement::Runner(name) => name,
      },
    }
  }
}

impl From<ProfileFinding> for v1::ProfileFinding {
  fn from(finding: ProfileFinding) -> Self {
    Self {
      diagnostic: finding.diagnostic.and_then(proto_diagnostic),
      severity: if finding.error {
        v1::FindingSeverity::Error
      } else {
        v1::FindingSeverity::Warning
      } as i32,
      message: finding.message,
    }
  }
}

impl From<ProfileCheck> for v1::CheckProfileResponse {
  fn from(check: ProfileCheck) -> Self {
    Self {
      actor: check.actor.map(Into::into),
      executors: check.executors.into_iter().map(|(name, p)| (name, p.into())).collect(),
      actor_cpu: check.actor_cpu,
      actor_image: check.actor_image,
      actor_base_abi: check.actor_base_abi.map(|abi| abi.to_string()).unwrap_or_default(),
      findings: check.findings.into_iter().map(Into::into).collect(),
    }
  }
}

pub fn proto_profile_check(check: ProfileCheck) -> v1::CheckProfileResponse {
  check.into()
}

impl From<RunWorkspace> for v1::RunWorkspace {
  fn from(w: RunWorkspace) -> Self {
    Self {
      path: w.path,
      executor: w.executor,
      source: w.source,
      writable: w.writable,
    }
  }
}

impl From<RunInfo> for v1::Run {
  fn from(run: RunInfo) -> Self {
    Self {
      run_id: run.id,
      name: run.name,
      group: run.group,
      state: proto_run_state(run.state).into(),
      executors: run
        .executors
        .into_iter()
        .map(|e| (e, false, false))
        .chain(run.released.into_iter().map(|e| (e, true, false)))
        .chain(run.lost.into_iter().map(|e| (e, true, true)))
        .map(|(e, released, lost)| v1::ExecutorRef {
          name: e.name,
          driver: e.driver,
          instance: e.instance,
          released,
          lost,
        })
        .collect(),
      created_at_ms: run.created_ms,
      profile: run.profile,
      actor_exit: run.actor_exit.map(proto_exit),
      tty: run.tty,
      failure: run.failure.unwrap_or_default(),
      actor_state: proto_actor_state(run.actor).into(),
      workspace: run.workspace.map(Into::into),
      snapshot: run.snapshot.unwrap_or_default(),
      commit_rebased: run.commit_rebased,
      resolved_features: run.resolved_features.into_iter().collect(),
    }
  }
}

pub fn proto_run(run: RunInfo) -> v1::Run {
  run.into()
}

/// The context of a command the actor's proxy asks about.
pub fn exec_context(request: v1::DecideRequest) -> ExecContext {
  ExecContext {
    argv: request.argv,
    cwd: request.cwd,
    env: request.env.into_iter().collect(),
    tty: request.tty,
    ancestors: request.ancestors.into_iter().map(|a| a.argv).collect(),
  }
}

impl From<Route> for v1::DecideResponse {
  fn from(route: Route) -> Self {
    let (target, deny, fault) = match route.target {
      Target::Actor => ("actor".to_string(), String::new(), None),
      Target::Executor(name) => (name, String::new(), None),
      Target::Refused(message) => (String::new(), message, None),
      Target::Fault(fault) => (
        String::new(),
        String::new(),
        Some(v1::ExecFault {
          name: fault.name,
          exit_code: fault.exit,
          signal: fault.signal.unwrap_or_default(),
          hang: fault.hang,
          stderr: fault.stderr,
        }),
      ),
    };
    Self {
      target,
      argv: route.argv,
      env: route.env,
      clear_env: route.clear_env,
      cwd: route.cwd,
      search_path_if_missing: route.search_path_if_missing,
      deny,
      fault,
    }
  }
}

pub fn proto_route(route: Route) -> v1::DecideResponse {
  route.into()
}
