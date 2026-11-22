//! Proto <-> domain mappings for `RunnerService`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use needy_api_proto::v1;
use needy_config::profile::{ActorDriver, ExecutorDriver};
use needy_core::types::{Capabilities, RunnerRole};

use crate::control::{JoinTokenInfo, JoinTokenState, Joining, NewJoinToken, RunnerHost, RunnerInfo, RunnerReport, RunnerStatus};

use super::Error;

/// The roles a request names; every one must be known and specified.
pub fn runner_roles(values: &[i32]) -> Result<BTreeSet<RunnerRole>, Error> {
  const FIELD: &str = "runner role";
  values
    .iter()
    .map(|&value| match v1::RunnerRole::try_from(value) {
      Ok(v1::RunnerRole::Unspecified) => Err(Error::Unspecified(FIELD)),
      Ok(v1::RunnerRole::Actor) => Ok(RunnerRole::Actor),
      Ok(v1::RunnerRole::Executor) => Ok(RunnerRole::Executor),
      Err(_) => Err(Error::Unknown {
        field: FIELD,
        value,
      }),
    })
    .collect()
}

pub fn proto_runner_role(role: RunnerRole) -> v1::RunnerRole {
  match role {
    RunnerRole::Actor => v1::RunnerRole::Actor,
    RunnerRole::Executor => v1::RunnerRole::Executor,
  }
}

fn proto_runner_roles(roles: &BTreeSet<RunnerRole>) -> Vec<i32> {
  roles.iter().map(|&role| proto_runner_role(role) as i32).collect()
}

/// The capabilities a runner reports; every actor and executor driver must be
/// known and specified. None reported is none at all.
pub fn capabilities(reported: Option<v1::RunnerCapabilities>) -> Result<Capabilities, Error> {
  let reported = reported.unwrap_or_default();
  let actor = reported
    .actor
    .iter()
    .map(|&value| match v1::ActorDriver::try_from(value) {
      Ok(v1::ActorDriver::Unspecified) => Err(Error::Unspecified("actor driver")),
      Ok(v1::ActorDriver::LinuxFirecracker) => Ok(ActorDriver::LinuxFirecracker),
      Ok(v1::ActorDriver::LinuxContainer) => Ok(ActorDriver::LinuxContainer),
      Ok(v1::ActorDriver::MacosVz) => Ok(ActorDriver::MacosVz),
      Ok(v1::ActorDriver::MacosAppleContainer) => Ok(ActorDriver::MacosAppleContainer),
      Err(_) => Err(Error::Unknown {
        field: "actor driver",
        value,
      }),
    })
    .collect::<Result<_, _>>()?;
  let executor = reported
    .executor
    .iter()
    .map(|&value| match v1::ExecutorDriver::try_from(value) {
      Ok(v1::ExecutorDriver::Unspecified) => Err(Error::Unspecified("executor driver")),
      Ok(v1::ExecutorDriver::Native) => Ok(ExecutorDriver::Native),
      Ok(v1::ExecutorDriver::LinuxContainer) => Ok(ExecutorDriver::LinuxContainer),
      Err(_) => Err(Error::Unknown {
        field: "executor driver",
        value,
      }),
    })
    .collect::<Result<_, _>>()?;
  Ok(Capabilities { actor, executor })
}

pub fn proto_capabilities(capabilities: &Capabilities) -> v1::RunnerCapabilities {
  let actor = |driver: &ActorDriver| match driver {
    ActorDriver::LinuxFirecracker => v1::ActorDriver::LinuxFirecracker as i32,
    ActorDriver::LinuxContainer => v1::ActorDriver::LinuxContainer as i32,
    ActorDriver::MacosVz => v1::ActorDriver::MacosVz as i32,
    ActorDriver::MacosAppleContainer => v1::ActorDriver::MacosAppleContainer as i32,
  };
  let executor = |driver: &ExecutorDriver| match driver {
    ExecutorDriver::Native => v1::ExecutorDriver::Native as i32,
    ExecutorDriver::LinuxContainer => v1::ExecutorDriver::LinuxContainer as i32,
  };
  v1::RunnerCapabilities {
    actor: capabilities.actor.iter().map(actor).collect(),
    executor: capabilities.executor.iter().map(executor).collect(),
  }
}

fn labels(labels: HashMap<String, String>) -> BTreeMap<String, String> {
  labels.into_iter().collect()
}

fn proto_labels(labels: BTreeMap<String, String>) -> HashMap<String, String> {
  labels.into_iter().collect()
}

impl TryFrom<v1::CreateJoinTokenRequest> for NewJoinToken {
  type Error = Error;

  fn try_from(request: v1::CreateJoinTokenRequest) -> Result<Self, Error> {
    Ok(Self {
      roles: runner_roles(&request.roles)?,
      ttl_ms: request.ttl_ms,
      uses: request.uses,
      labels: labels(request.labels),
    })
  }
}

pub fn new_join_token(request: v1::CreateJoinTokenRequest) -> Result<NewJoinToken, Error> {
  request.try_into()
}

// NOTICE: proto -> `JoinTokenState` and `RunnerStatus` live on the client
// side (`needy-cli`), the only reader of them on the wire.
impl From<JoinTokenInfo> for v1::JoinToken {
  fn from(token: JoinTokenInfo) -> Self {
    let state = match token.state {
      JoinTokenState::Valid => v1::JoinTokenState::Valid,
      JoinTokenState::Expired => v1::JoinTokenState::Expired,
      JoinTokenState::UsedUp => v1::JoinTokenState::UsedUp,
      JoinTokenState::Revoked => v1::JoinTokenState::Revoked,
    };
    Self {
      id: token.id,
      roles: proto_runner_roles(&token.roles),
      labels: proto_labels(token.labels),
      created_at_ms: token.created_ms,
      expires_at_ms: token.expires_ms,
      uses_left: token.uses_left,
      state: state as i32,
    }
  }
}

pub fn proto_join_token(token: JoinTokenInfo) -> v1::JoinToken {
  token.into()
}

impl TryFrom<v1::JoinRequest> for Joining {
  type Error = Error;

  fn try_from(request: v1::JoinRequest) -> Result<Self, Error> {
    if request.join_token.is_empty() {
      return Err(Error::Missing("join token"));
    }
    if request.name.is_empty() {
      return Err(Error::Missing("runner name"));
    }
    Ok(Self {
      report: RunnerReport {
        capabilities: capabilities(request.capabilities)?,
        labels: labels(request.labels),
        host: None,
      },
      join_token: request.join_token,
      name: request.name,
    })
  }
}

pub fn joining(request: v1::JoinRequest) -> Result<Joining, Error> {
  request.try_into()
}

impl TryFrom<v1::HeartbeatRequest> for RunnerReport {
  type Error = Error;

  fn try_from(request: v1::HeartbeatRequest) -> Result<Self, Error> {
    Ok(Self {
      capabilities: capabilities(request.capabilities)?,
      labels: labels(request.labels),
      host: request.host.map(Into::into),
    })
  }
}

pub fn runner_report(request: v1::HeartbeatRequest) -> Result<RunnerReport, Error> {
  request.try_into()
}

impl From<v1::RunnerHost> for RunnerHost {
  fn from(host: v1::RunnerHost) -> Self {
    Self {
      os: host.os,
      arch: host.arch,
      cpu: needy_core::recipe::HostInfo {
        flags: host.cpu_flags.into_iter().collect(),
        vendor: host.cpu_vendor,
        model_name: host.cpu_model,
        kvm_cpuid: host.kvm_cpuid.map(|cpuid| cpuid.entries.into_iter().map(|e| (e.leaf, e.subleaf)).collect()),
      },
      home: host.home,
    }
  }
}

pub fn runner_host(host: v1::RunnerHost) -> RunnerHost {
  host.into()
}

impl From<&RunnerHost> for v1::RunnerHost {
  fn from(host: &RunnerHost) -> Self {
    Self {
      os: host.os.clone(),
      arch: host.arch.clone(),
      cpu_vendor: host.cpu.vendor.clone(),
      cpu_model: host.cpu.model_name.clone(),
      cpu_flags: host.cpu.flags.iter().cloned().collect(),
      kvm_cpuid: host.cpu.kvm_cpuid.as_ref().map(|entries| v1::KvmCpuid {
        entries: entries.iter().map(|&(leaf, subleaf)| v1::CpuidEntry { leaf, subleaf }).collect(),
      }),
      home: host.home.clone(),
    }
  }
}

/// A runner's machine as its heartbeats carry it.
pub fn proto_runner_host(host: &RunnerHost) -> v1::RunnerHost {
  host.into()
}

impl From<RunnerInfo> for v1::Runner {
  fn from(runner: RunnerInfo) -> Self {
    let status = match runner.status {
      RunnerStatus::Online => v1::RunnerStatus::Online,
      RunnerStatus::Offline => v1::RunnerStatus::Offline,
      RunnerStatus::Removed => v1::RunnerStatus::Removed,
    };
    Self {
      capabilities: Some(proto_capabilities(&runner.capabilities)),
      host: runner.host.as_ref().map(Into::into),
      name: runner.name,
      labels: proto_labels(runner.labels),
      status: status as i32,
      created_at_ms: runner.created_ms,
      last_seen_at_ms: runner.last_seen_ms,
      rotating: runner.rotating,
      join_token_id: runner.join_token,
    }
  }
}

pub fn proto_runner(runner: RunnerInfo) -> v1::Runner {
  runner.into()
}
