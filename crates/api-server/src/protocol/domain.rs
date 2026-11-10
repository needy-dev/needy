//! Mappings between generated types and the domain types of `control` and
//! `needy_core`, one function per direction. The generated types never reach
//! `needy_core`.
//!
//! A proto enum arrives as an `i32`. Its zero value means the sender left
//! the field unset, and a value this build does not know comes from a newer
//! schema; both are refused instead of read as a default.
//!
//! The daemon relays execs to executors through `needy-api-client`, whose
//! requests are generated types too; [`proto_exec_start`], [`started`] and
//! [`process_info`] are the outbound side of that relay.

use needy_api_proto::v1;
use needy_common::process::Exit;
use needy_core::event::Event;
use needy_core::intercept::ExecContext;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use needy_core::types::{Origin, RunState, RunnerRole};

use crate::control::{
  ClientInfo, ExecInput, ExecOutput, ExecSpec, GuestConfig, JoinTokenInfo, JoinTokenState, Joining, NewJoinToken, NewRun, Placement,
  ProcessInfo, ProfileCheck, ProfileToCheck, Route, RunInfo, RunnerHost, RunnerInfo, RunnerReport, RunnerStatus, Started, StoredEvent,
  Target, WindowSize,
};

/// A request field with no domain meaning.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
  #[error("the {0} is unspecified")]
  Unspecified(&'static str),
  #[error("unknown {field} {value}")]
  Unknown { field: &'static str, value: i32 },
  #[error("the request names no {0}")]
  Missing(&'static str),
}

/// The origin an `ExecStart` names.
pub fn origin(value: i32) -> Result<Origin, Error> {
  const FIELD: &str = "exec origin";
  match v1::Origin::try_from(value) {
    Ok(v1::Origin::Unspecified) => Err(Error::Unspecified(FIELD)),
    Ok(v1::Origin::Cli) => Ok(Origin::Cli),
    Ok(v1::Origin::Fs) => Ok(Origin::Fs),
    Ok(v1::Origin::Files) => Ok(Origin::Files),
    Ok(v1::Origin::ActorProxy) => Ok(Origin::ActorProxy),
    Ok(v1::Origin::ActorMount) => Ok(Origin::ActorMount),
    Err(_) => Err(Error::Unknown {
      field: FIELD,
      value,
    }),
  }
}

pub fn proto_origin(origin: Origin) -> v1::Origin {
  match origin {
    Origin::Cli => v1::Origin::Cli,
    Origin::Fs => v1::Origin::Fs,
    Origin::Files => v1::Origin::Files,
    Origin::ActorProxy => v1::Origin::ActorProxy,
    Origin::ActorMount => v1::Origin::ActorMount,
  }
}

// NOTICE: proto -> `RunState` lives on the client side
// (`needy-cli`), the only reader of run states on the wire.
pub fn proto_run_state(state: RunState) -> v1::RunState {
  match state {
    RunState::Creating => v1::RunState::Creating,
    RunState::Running => v1::RunState::Running,
    RunState::Stopping => v1::RunState::Stopping,
    RunState::Stopped => v1::RunState::Stopped,
    RunState::Finished => v1::RunState::Finished,
    RunState::Failed => v1::RunState::Failed,
  }
}

// -------------------------------------------------------------- runners

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

fn labels(labels: HashMap<String, String>) -> BTreeMap<String, String> {
  labels.into_iter().collect()
}

fn proto_labels(labels: BTreeMap<String, String>) -> HashMap<String, String> {
  labels.into_iter().collect()
}

pub fn new_join_token(request: v1::CreateJoinTokenRequest) -> Result<NewJoinToken, Error> {
  Ok(NewJoinToken {
    roles: runner_roles(&request.roles)?,
    ttl_ms: request.ttl_ms,
    uses: request.uses,
    labels: labels(request.labels),
  })
}

// NOTICE: proto -> `JoinTokenState` and `RunnerStatus` live on the client
// side (`needy-cli`), the only reader of them on the wire.
pub fn proto_join_token(token: JoinTokenInfo) -> v1::JoinToken {
  let state = match token.state {
    JoinTokenState::Valid => v1::JoinTokenState::Valid,
    JoinTokenState::Expired => v1::JoinTokenState::Expired,
    JoinTokenState::UsedUp => v1::JoinTokenState::UsedUp,
    JoinTokenState::Revoked => v1::JoinTokenState::Revoked,
  };
  v1::JoinToken {
    id: token.id,
    roles: proto_runner_roles(&token.roles),
    labels: proto_labels(token.labels),
    created_at_ms: token.created_ms,
    expires_at_ms: token.expires_ms,
    uses_left: token.uses_left,
    state: state as i32,
  }
}

pub fn proto_client(client: ClientInfo) -> v1::Client {
  v1::Client {
    id: client.id,
    name: client.name,
    created_at_ms: client.created_ms,
    last_used_at_ms: client.last_used_ms.unwrap_or(0),
    revoked_at_ms: client.revoked_ms.unwrap_or(0),
  }
}

pub fn proto_stored_event(event: StoredEvent) -> v1::StoredEvent {
  v1::StoredEvent {
    seq: event.seq,
    time_ms: event.time_ms,
    kind: event.kind,
    version: event.version,
    payload: event.payload,
  }
}

pub fn joining(request: v1::JoinRequest) -> Result<Joining, Error> {
  if request.join_token.is_empty() {
    return Err(Error::Missing("join token"));
  }
  if request.name.is_empty() {
    return Err(Error::Missing("runner name"));
  }
  Ok(Joining {
    report: RunnerReport {
      roles: runner_roles(&request.roles)?,
      labels: labels(request.labels),
      host: None,
    },
    join_token: request.join_token,
    name: request.name,
  })
}

pub fn runner_report(request: v1::HeartbeatRequest) -> Result<RunnerReport, Error> {
  Ok(RunnerReport {
    roles: runner_roles(&request.roles)?,
    labels: labels(request.labels),
    host: request.host.map(runner_host),
  })
}

fn runner_host(host: v1::RunnerHost) -> RunnerHost {
  RunnerHost {
    os: host.os,
    arch: host.arch,
    cpu: needy_core::recipe::HostInfo {
      flags: host.cpu_flags.into_iter().collect(),
      vendor: host.cpu_vendor,
      model_name: host.cpu_model,
      kvm_cpuid: host.kvm_cpuid.map(|cpuid| cpuid.entries.into_iter().map(|e| (e.leaf, e.subleaf)).collect()),
    },
  }
}

/// A runner's machine as its heartbeats carry it.
pub fn proto_runner_host(host: &RunnerHost) -> v1::RunnerHost {
  v1::RunnerHost {
    os: host.os.clone(),
    arch: host.arch.clone(),
    cpu_vendor: host.cpu.vendor.clone(),
    cpu_model: host.cpu.model_name.clone(),
    cpu_flags: host.cpu.flags.iter().cloned().collect(),
    kvm_cpuid: host.cpu.kvm_cpuid.as_ref().map(|entries| v1::KvmCpuid {
      entries: entries.iter().map(|&(leaf, subleaf)| v1::CpuidEntry { leaf, subleaf }).collect(),
    }),
  }
}

pub fn proto_runner(runner: RunnerInfo) -> v1::Runner {
  let status = match runner.status {
    RunnerStatus::Online => v1::RunnerStatus::Online,
    RunnerStatus::Offline => v1::RunnerStatus::Offline,
    RunnerStatus::Removed => v1::RunnerStatus::Removed,
  };
  v1::Runner {
    roles: proto_runner_roles(&runner.roles),
    name: runner.name,
    labels: proto_labels(runner.labels),
    status: status as i32,
    created_at_ms: runner.created_ms,
    last_seen_at_ms: runner.last_seen_ms,
    rotating: runner.rotating,
    join_token_id: runner.join_token,
  }
}

// ------------------------------------------------------------- runs

pub fn new_run(request: v1::CreateRunRequest) -> Result<NewRun, Error> {
  if request.profile_name.is_empty() {
    return Err(Error::Missing("profile"));
  }
  Ok(NewRun {
    profile_yaml: request.profile_yaml,
    recipe_yaml: request.recipe_yaml,
    profile_name: request.profile_name,
    group: request.group,
    name: request.name,
    actor_command: request.actor_command,
    // The adapter sets who asks; the wire does not say.
    by: None,
  })
}

pub fn profile_to_check(request: v1::CheckProfileRequest) -> ProfileToCheck {
  ProfileToCheck {
    profile_yaml: request.profile_yaml,
    recipe_yaml: request.recipe_yaml,
  }
}

pub fn proto_profile_check(check: ProfileCheck) -> v1::CheckProfileResponse {
  let placement = |placement: Placement| v1::Placement {
    runner: match placement {
      Placement::ControlHost => String::new(),
      Placement::Runner(name) => name,
    },
  };
  v1::CheckProfileResponse {
    actor: check.actor.map(placement),
    executors: check.executors.into_iter().map(|(name, p)| (name, placement(p))).collect(),
    actor_cpu: check.actor_cpu,
    findings: check
      .findings
      .into_iter()
      .map(|finding| v1::ProfileFinding {
        severity: if finding.error {
          v1::FindingSeverity::Error
        } else {
          v1::FindingSeverity::Warning
        } as i32,
        message: finding.message,
      })
      .collect(),
  }
}

pub fn proto_run(run: RunInfo) -> v1::Run {
  v1::Run {
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
        runtime: e.runtime,
        instance: e.instance,
        released,
        lost,
      })
      .collect(),
    created_at_ms: run.created_ms,
    profile: run.profile,
    actor_exit: run.actor_exit.map(proto_exit),
    failure: run.failure.unwrap_or_default(),
  }
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

pub fn proto_route(route: Route) -> v1::DecideResponse {
  let (target, deny) = match route.target {
    Target::Actor => ("actor".to_string(), String::new()),
    Target::Executor(name) => (name, String::new()),
    Target::Refused(message) => (String::new(), message),
  };
  v1::DecideResponse {
    target,
    argv: route.argv,
    env: route.env,
    clear_env: route.clear_env,
    cwd: route.cwd,
    search_path_if_missing: route.search_path_if_missing,
    deny,
  }
}

// ------------------------------------------------------------ processes

pub fn exec_spec(start: v1::ExecStart) -> Result<ExecSpec, Error> {
  Ok(ExecSpec {
    origin: origin(start.origin)?,
    argv: start.argv,
    cwd: start.cwd,
    env: start.env,
    clear_env: start.clear_env,
    tty: start.tty.map(|t| WindowSize {
      rows: t.rows,
      cols: t.cols,
    }),
    detach: start.detach,
    run: start.run_id,
    executor: start.executor,
    runner: start.runner,
    no_stdin: start.no_stdin,
    search_path_if_missing: start.search_path_if_missing,
    // The adapter sets who asks; the wire does not say.
    by: None,
  })
}

pub fn proto_exec_start(spec: ExecSpec) -> v1::ExecStart {
  v1::ExecStart {
    argv: spec.argv,
    cwd: spec.cwd,
    env: spec.env,
    clear_env: spec.clear_env,
    tty: spec.tty.map(proto_window),
    detach: spec.detach,
    run_id: spec.run,
    executor: spec.executor,
    runner: spec.runner,
    no_stdin: spec.no_stdin,
    origin: proto_origin(spec.origin).into(),
    search_path_if_missing: spec.search_path_if_missing,
  }
}

pub fn proto_window(size: WindowSize) -> v1::WindowSize {
  v1::WindowSize {
    rows: size.rows,
    cols: size.cols,
  }
}

/// A request after the first; a second start or attach has no meaning and
/// maps to nothing.
pub fn exec_input(payload: v1::exec_request::Payload) -> Option<ExecInput> {
  use v1::exec_request::Payload;
  match payload {
    Payload::Stdin(bytes) => Some(ExecInput::Stdin(bytes)),
    Payload::StdinEof(_) => Some(ExecInput::Eof),
    Payload::Signal(name) => Some(ExecInput::Signal(name)),
    Payload::Resize(size) => Some(ExecInput::Resize(WindowSize {
      rows: size.rows,
      cols: size.cols,
    })),
    Payload::Detach(_) => Some(ExecInput::Detach),
    Payload::Start(_) | Payload::Attach(_) => None,
  }
}

pub fn proto_exec_output(output: ExecOutput) -> v1::ExecResponse {
  use v1::exec_response::Payload;
  let payload = match output {
    ExecOutput::Trace(event) => Payload::Event(proto_exec_event(&event)),
    ExecOutput::Started(started) => Payload::Started(v1::ExecStarted {
      process_id: started.process,
      executor: started.executor,
      pid: started.pid,
      argv: started.argv,
      cwd: started.cwd,
      // TODO(term-fallback): the control daemon records a terminal type
      // fallback in the trace's `exec.start`; its callers do not get it yet,
      // as no client shows it. Add it to `Started` when one does.
      term_fallback: None,
    }),
    ExecOutput::Stdout(bytes) => Payload::Stdout(bytes),
    ExecOutput::Stderr(bytes) => Payload::Stderr(bytes),
    ExecOutput::Exited(exit) => Payload::Exited(proto_exit(exit)),
    ExecOutput::Error(message) => Payload::Error(message),
    ExecOutput::Ended(reason) => Payload::Ended(v1::ExecEnded { reason }),
    ExecOutput::Detached => Payload::Detached(true),
  };
  v1::ExecResponse {
    payload: Some(payload),
  }
}

/// A trace event in the form the exec stream carries it.
pub fn proto_exec_event(event: &Event) -> v1::ExecEvent {
  v1::ExecEvent {
    kind: event.kind().as_str().into(),
    payload: event.payload(),
  }
}

pub fn started(started: v1::ExecStarted) -> Started {
  Started {
    process: started.process_id,
    executor: started.executor,
    pid: started.pid,
    argv: started.argv,
    cwd: started.cwd,
  }
}

pub fn process_info(info: v1::ProcessInfo) -> ProcessInfo {
  ProcessInfo {
    process: info.process_id,
    argv: info.argv,
    executor: info.executor,
    pid: info.pid,
    running: info.running,
    exit: info.exited.map(exit),
    tty: info.tty,
    started_ms: info.started_at_ms,
    run: info.run_id,
    // An executor does not keep the origin; unspecified means unknown.
    origin: origin(info.origin).ok(),
  }
}

pub fn proto_process_info(info: ProcessInfo) -> v1::ProcessInfo {
  v1::ProcessInfo {
    process_id: info.process,
    argv: info.argv,
    executor: info.executor,
    pid: info.pid,
    running: info.running,
    exited: info.exit.map(proto_exit),
    tty: info.tty,
    started_at_ms: info.started_ms,
    run_id: info.run,
    origin: info.origin.map_or(v1::Origin::Unspecified, proto_origin).into(),
  }
}

pub fn exit(exited: v1::ExecExited) -> Exit {
  Exit {
    code: exited.code,
    signal: exited.signal,
  }
}

pub fn proto_exit(exit: Exit) -> v1::ExecExited {
  v1::ExecExited {
    code: exit.code,
    signal: exit.signal,
  }
}

// ---------------------------------------------------------------- guest

pub fn proto_guest_config(config: GuestConfig) -> v1::GetConfigResponse {
  v1::GetConfigResponse {
    run_id: config.run,
    user: Some(v1::GuestUser {
      name: config.user.name,
      uid: config.user.uid,
      gid: config.user.gid,
    }),
    route_names: config.route_names,
    hostname: config.hostname,
    uptime_offset_secs: config.uptime_offset_secs,
    mounts: config
      .mounts
      .into_iter()
      .map(|m| v1::GuestMount {
        target: m.target,
        executor: m.executor,
        source: m.source,
        writable: m.writable,
        sftp_command: m.sftp_command,
      })
      .collect(),
    command: config.command,
    env: config.env,
    cwd: config.cwd,
    gateway_port: config.gateway_port,
    sensors: config
      .sensors
      .into_iter()
      .map(|s| v1::GuestSensor {
        chip: s.chip,
        idle_mc: s.idle_mc,
      })
      .collect(),
    copy_in: config
      .copy_in
      .into_iter()
      .map(|c| v1::GuestCopy {
        name: c.name,
        path: c.path,
      })
      .collect(),
    mirror_out: config
      .mirror_out
      .into_iter()
      .map(|c| v1::GuestCopy {
        name: c.name,
        path: c.path,
      })
      .collect(),
    hwmon_faults: config
      .hwmon_faults
      .into_iter()
      .map(|f| v1::GuestFault {
        at_ms: f.at_ms,
        temp_mc: f.temp_mc,
      })
      .collect(),
  }
}

#[cfg(test)]
#[path = "domain_test.rs"]
mod tests;
