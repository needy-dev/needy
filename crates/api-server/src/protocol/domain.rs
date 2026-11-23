//! Mappings between generated types and the domain types of `control` and
//! `needy_core`. Local `control::*` ↔ `v1::*` conversions implement `From` /
//! `TryFrom`; foreign types (`needy_core`, `needy_common`, `needy_config`) use
//! free functions. The generated types never reach `needy_core`.
//!
//! A proto enum arrives as an `i32`. Its zero value means the sender left
//! the field unset, and a value this build does not know comes from a newer
//! schema; both are refused instead of read as a default.
//!
//! The daemon relays execs to executors through `needy-api-client`, whose
//! requests are generated types too; [`proto_exec_start`], [`started`] and
//! [`process_info`] are the outbound side of that relay.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use needy_api_proto::v1;
use needy_common::process::Exit;
use needy_config::profile::{ActorDriver, ExecutorDriver};
use needy_core::event::{ActorStop, Event};
use needy_core::intercept::exec::ExecContext;
use needy_core::types::{ActorState, Capabilities, Origin, RunState, RunnerRole};

use crate::control::{
  BuildStepOutcome, ClientAccess, ClientInfo, CommitHistoryEntry, Diagnostic, ExecInput, ExecOutput, ExecSpec, GuestConfig, GuestCopy,
  GuestFault, GuestMount, GuestSensor, GuestSession, GuestUser, ImageBuild, ImageBuildResult, ImageEditFinished, ImageEditStart,
  ImageEditStarted, ImageExport, ImageLog, ImageLogResult, ImageRollback, ImageRollbackResult, JoinTokenInfo, JoinTokenState, Joining,
  NewJoinToken, NewRun, Placement, ProcessInfo, ProfileCheck, ProfileFinding, ProfileToCheck, RecordedCommand, Route, RunFaults, RunInfo,
  RunWorkspace, RunnerHost, RunnerInfo, RunnerReport, RunnerStatus, Started, StopMode, StoredEvent, Target, UpperLayer, WindowSize,
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
  #[error("{0}")]
  Contradicts(&'static str),
  #[error("{0}")]
  Invalid(String),
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

pub fn proto_actor_state(state: Option<ActorState>) -> v1::ActorState {
  match state {
    None => v1::ActorState::Unspecified,
    Some(ActorState::Starting) => v1::ActorState::Starting,
    Some(ActorState::Configured) => v1::ActorState::Configured,
    Some(ActorState::Ready) => v1::ActorState::Ready,
    Some(ActorState::Exited) => v1::ActorState::Exited,
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

impl From<ClientInfo> for v1::Client {
  fn from(client: ClientInfo) -> Self {
    Self {
      access: v1::ClientAccess::from(client.access).into(),
      id: client.id,
      name: client.name,
      created_at_ms: client.created_ms,
      last_used_at_ms: client.last_used_ms.unwrap_or(0),
      revoked_at_ms: client.revoked_ms.unwrap_or(0),
    }
  }
}

pub fn proto_client(client: ClientInfo) -> v1::Client {
  client.into()
}

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

// ------------------------------------------------------------- runs

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

// ------------------------------------------------------------ processes

impl From<v1::WindowSize> for WindowSize {
  fn from(size: v1::WindowSize) -> Self {
    Self {
      rows: size.rows,
      cols: size.cols,
    }
  }
}

impl From<WindowSize> for v1::WindowSize {
  fn from(size: WindowSize) -> Self {
    Self {
      rows: size.rows,
      cols: size.cols,
    }
  }
}

pub fn proto_window(size: WindowSize) -> v1::WindowSize {
  size.into()
}

impl TryFrom<v1::ExecStart> for ExecSpec {
  type Error = Error;

  fn try_from(start: v1::ExecStart) -> Result<Self, Error> {
    Ok(Self {
      origin: origin(start.origin)?,
      argv: start.argv,
      cwd: start.cwd,
      env: start.env,
      clear_env: start.clear_env,
      tty: start.tty.map(Into::into),
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
}

pub fn exec_spec(start: v1::ExecStart) -> Result<ExecSpec, Error> {
  start.try_into()
}

impl From<ExecSpec> for v1::ExecStart {
  fn from(spec: ExecSpec) -> Self {
    Self {
      argv: spec.argv,
      cwd: spec.cwd,
      env: spec.env,
      clear_env: spec.clear_env,
      tty: spec.tty.map(Into::into),
      detach: spec.detach,
      run_id: spec.run,
      executor: spec.executor,
      runner: spec.runner,
      no_stdin: spec.no_stdin,
      origin: proto_origin(spec.origin).into(),
      search_path_if_missing: spec.search_path_if_missing,
    }
  }
}

pub fn proto_exec_start(spec: ExecSpec) -> v1::ExecStart {
  spec.into()
}

/// A request after the first; a second start or attach has no meaning and
/// maps to nothing.
pub fn exec_input(payload: v1::exec_request::Payload) -> Option<ExecInput> {
  use v1::exec_request::Payload;
  match payload {
    Payload::Stdin(bytes) => Some(ExecInput::Stdin(bytes)),
    Payload::StdinEof(_) => Some(ExecInput::Eof),
    Payload::Signal(name) => Some(ExecInput::Signal(name)),
    Payload::Resize(size) => Some(ExecInput::Resize(size.into())),
    Payload::Detach(_) => Some(ExecInput::Detach),
    Payload::Start(_) | Payload::LimitedStart(_) | Payload::Attach(_) => None,
  }
}

impl From<ExecOutput> for v1::ExecResponse {
  fn from(output: ExecOutput) -> Self {
    if let ExecOutput::DiagnosticError {
      message,
      diagnostic,
    } = output
    {
      return Self {
        payload: Some(v1::exec_response::Payload::Error(message)),
        diagnostic: proto_diagnostic(diagnostic),
      };
    }
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
      ExecOutput::DiagnosticError { .. } => unreachable!("handled above"),
      ExecOutput::Ended(reason) => Payload::Ended(v1::ExecEnded { reason }),
      ExecOutput::Detached => Payload::Detached(true),
    };
    Self {
      diagnostic: None,
      payload: Some(payload),
    }
  }
}

pub fn proto_exec_output(output: ExecOutput) -> v1::ExecResponse {
  output.into()
}

/// A trace event in the form the exec stream carries it.
pub fn proto_exec_event(event: &Event) -> v1::ExecEvent {
  v1::ExecEvent {
    kind: event.kind().as_str().into(),
    payload: event.payload(),
  }
}

impl From<v1::ExecStarted> for Started {
  fn from(started: v1::ExecStarted) -> Self {
    Self {
      process: started.process_id,
      executor: started.executor,
      pid: started.pid,
      argv: started.argv,
      cwd: started.cwd,
    }
  }
}

pub fn started(started: v1::ExecStarted) -> Started {
  started.into()
}

impl From<v1::ProcessInfo> for ProcessInfo {
  fn from(info: v1::ProcessInfo) -> Self {
    Self {
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
}

pub fn process_info(info: v1::ProcessInfo) -> ProcessInfo {
  info.into()
}

impl From<ProcessInfo> for v1::ProcessInfo {
  fn from(info: ProcessInfo) -> Self {
    Self {
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
}

pub fn proto_process_info(info: ProcessInfo) -> v1::ProcessInfo {
  info.into()
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

impl From<GuestUser> for v1::GuestUser {
  fn from(user: GuestUser) -> Self {
    Self {
      name: user.name,
      uid: user.uid,
      gid: user.gid,
    }
  }
}

impl From<GuestMount> for v1::GuestMount {
  fn from(m: GuestMount) -> Self {
    Self {
      target: m.target,
      executor: m.executor,
      source: m.source,
      writable: m.writable,
      sftp_command: m.sftp_command,
    }
  }
}

impl From<GuestSensor> for v1::GuestSensor {
  fn from(s: GuestSensor) -> Self {
    Self {
      chip: s.chip,
      idle_mc: s.idle_mc,
    }
  }
}

impl From<GuestCopy> for v1::GuestCopy {
  fn from(c: GuestCopy) -> Self {
    Self {
      name: c.name,
      path: c.path,
    }
  }
}

impl From<GuestFault> for v1::GuestFault {
  fn from(f: GuestFault) -> Self {
    Self {
      at_ms: f.at_ms,
      temp_mc: f.temp_mc,
      name: f.name,
    }
  }
}

impl From<GuestSession> for v1::GuestSession {
  fn from(session: GuestSession) -> Self {
    Self {
      upper: match session.upper {
        UpperLayer::Disposable => v1::UpperLayer::Disposable,
        UpperLayer::Kept => v1::UpperLayer::Kept,
      }
      .into(),
      routing: session.routing,
    }
  }
}

impl From<GuestConfig> for v1::GetConfigResponse {
  fn from(config: GuestConfig) -> Self {
    Self {
      tty: config.tty.map(Into::into),
      process_port: config.process_port,
      wait_for_attach: config.wait_for_attach,
      daemon_uid: config.daemon_uid,
      daemon_gid: config.daemon_gid,
      run_id: config.run,
      user: Some(config.user.into()),
      route_names: config.route_names,
      hostname: config.hostname,
      machine_id: config.machine_id,
      uptime_offset_secs: config.uptime_offset_secs,
      mounts: config.mounts.into_iter().map(Into::into).collect(),
      command: config.command,
      env: config.env,
      cwd: config.cwd,
      gateway_port: config.gateway_port,
      sensors: config.sensors.into_iter().map(Into::into).collect(),
      copy_in: config.copy_in.into_iter().map(Into::into).collect(),
      mirror_out: config.mirror_out.into_iter().map(Into::into).collect(),
      sensor_faults: config.sensor_faults.into_iter().map(Into::into).collect(),
      layers: config.layers.into_iter().map(|digest| v1::GuestLayer { digest }).collect(),
      image_path: config.image_path,
      session: Some(config.session.into()),
      start_hooks: config.start_hooks,
      stop_hooks: config.stop_hooks,
    }
  }
}

pub fn proto_guest_config(config: GuestConfig) -> v1::GetConfigResponse {
  config.into()
}

/// Client grants must be explicit; unspecified and unknown grants fail closed.
impl TryFrom<i32> for ClientAccess {
  type Error = Error;

  fn try_from(value: i32) -> Result<Self, Error> {
    match v1::ClientAccess::try_from(value) {
      Ok(v1::ClientAccess::Runs) => Ok(Self::Runs),
      Ok(v1::ClientAccess::Unspecified) => Err(Error::Unspecified("client access")),
      Ok(v1::ClientAccess::Admin) => Ok(Self::Admin),
      Err(_) => Err(Error::Unknown {
        field: "client access",
        value,
      }),
    }
  }
}

pub fn client_access(value: i32) -> Result<ClientAccess, Error> {
  value.try_into()
}

impl From<ClientAccess> for v1::ClientAccess {
  fn from(value: ClientAccess) -> Self {
    match value {
      ClientAccess::Runs => Self::Runs,
      ClientAccess::Admin => Self::Admin,
    }
  }
}

pub fn proto_client_access(value: ClientAccess) -> v1::ClientAccess {
  value.into()
}

/// Accepts only the known bounded wire detail; malformed facts are discarded.
pub fn diagnostic(value: v1::ErrorDiagnostic) -> Option<Diagnostic> {
  let value = needy_api_proto::diagnostic::validated(&value)?;
  Some(Diagnostic {
    reason: value.reason,
    facts: value.facts.into_iter().collect(),
  })
}

pub fn proto_diagnostic(value: Diagnostic) -> Option<v1::ErrorDiagnostic> {
  needy_api_proto::diagnostic::validated(&v1::ErrorDiagnostic {
    reason: value.reason,
    facts: value.facts.into_iter().collect(),
  })
}

// --------------------------------------------------------------- images

pub fn image_commit(commit: v1::ImageCommit) -> Result<needy_core::image::Commit, Error> {
  let layer = needy_core::image::Digest::parse(&commit.layer).map_err(|e| Error::Invalid(format!("image commit layer: {e}")))?;
  let base = needy_core::image::Digest::parse(&commit.base).map_err(|e| Error::Invalid(format!("image commit base: {e}")))?;
  Ok(needy_core::image::Commit { layer, base })
}

pub fn proto_image_commit(commit: needy_core::image::Commit) -> v1::ImageCommit {
  v1::ImageCommit {
    layer: commit.layer.to_string(),
    base: commit.base.to_string(),
  }
}

fn image_user(user: v1::ImageUser) -> needy_config::profile::User {
  needy_config::profile::User {
    name: user.name,
    uid: user.uid,
    gid: user.gid,
  }
}

fn pinned_features(raw: HashMap<String, String>) -> Result<BTreeMap<String, String>, Error> {
  let mut out = BTreeMap::new();
  for (reference, digest) in raw {
    if reference.trim().is_empty() {
      return Err(Error::Invalid("pinned feature reference cannot be empty".into()));
    }
    needy_core::image::Digest::parse(&digest).map_err(|e| Error::Invalid(format!("pinned feature `{reference}` digest: {e}")))?;
    out.insert(reference, digest);
  }
  Ok(out)
}

impl TryFrom<v1::BuildImageRequest> for ImageBuild {
  type Error = Error;

  fn try_from(request: v1::BuildImageRequest) -> Result<Self, Error> {
    if request.image_yaml.trim().is_empty() {
      return Err(Error::Missing("image definition"));
    }
    Ok(Self {
      image_yaml: request.image_yaml,
      driver: request.driver,
      user: request.user.map(image_user),
      commit: request.commit.map(image_commit).transpose()?,
      pinned_features: pinned_features(request.pinned_features)?,
    })
  }
}

pub fn image_build(request: v1::BuildImageRequest) -> Result<ImageBuild, Error> {
  request.try_into()
}

impl From<BuildStepOutcome> for v1::BuildStepResult {
  fn from(s: BuildStepOutcome) -> Self {
    Self {
      key: s.key,
      layer: s.layer,
      cached: s.cached,
      ms: s.ms,
    }
  }
}

impl From<ImageBuildResult> for v1::BuildImageResponse {
  fn from(result: ImageBuildResult) -> Self {
    Self {
      snapshot: result.snapshot,
      commit_rebased: result.commit_rebased,
      steps: result.steps.into_iter().map(Into::into).collect(),
      resolved_features: result.resolved_features.into_iter().collect(),
    }
  }
}

pub fn proto_image_build(result: ImageBuildResult) -> v1::BuildImageResponse {
  result.into()
}

impl TryFrom<v1::StartImageEditRequest> for ImageEditStart {
  type Error = Error;

  fn try_from(request: v1::StartImageEditRequest) -> Result<Self, Error> {
    if request.image_name.trim().is_empty() {
      return Err(Error::Missing("image name"));
    }
    if request.image_yaml.trim().is_empty() {
      return Err(Error::Missing("image definition"));
    }
    Ok(Self {
      image_name: request.image_name,
      image_yaml: request.image_yaml,
      driver: request.driver,
      user: request.user.map(image_user),
      commit: request.commit.map(image_commit).transpose()?,
      tty: request.tty.map(Into::into),
      wait_for_attach: request.wait_for_attach,
      pinned_features: pinned_features(request.pinned_features)?,
    })
  }
}

pub fn image_edit_start(request: v1::StartImageEditRequest) -> Result<ImageEditStart, Error> {
  request.try_into()
}

impl From<ImageEditStarted> for v1::StartImageEditResponse {
  fn from(started: ImageEditStarted) -> Self {
    Self {
      session_id: started.session_id,
      commit_rebased: started.commit_rebased,
      resolved_features: started.resolved_features.into_iter().collect(),
    }
  }
}

pub fn proto_image_edit_started(started: ImageEditStarted) -> v1::StartImageEditResponse {
  started.into()
}

impl From<RecordedCommand> for v1::RecordedCommand {
  fn from(cmd: RecordedCommand) -> Self {
    Self {
      argv: cmd.argv,
      cwd: cmd.cwd,
    }
  }
}

impl From<ImageEditFinished> for v1::FinishImageEditResponse {
  fn from(finished: ImageEditFinished) -> Self {
    Self {
      commit: finished.commit.map(proto_image_commit),
      commands: finished.commands.into_iter().map(Into::into).collect(),
      exit: finished.exit.map(proto_exit),
    }
  }
}

pub fn proto_image_edit_finished(finished: ImageEditFinished) -> v1::FinishImageEditResponse {
  finished.into()
}

impl TryFrom<v1::RollbackImageRequest> for ImageRollback {
  type Error = Error;

  fn try_from(request: v1::RollbackImageRequest) -> Result<Self, Error> {
    if request.image_name.trim().is_empty() {
      return Err(Error::Missing("image name"));
    }
    if request.from_layer.trim().is_empty() {
      return Err(Error::Missing("current commit layer"));
    }
    if request.steps == 0 {
      return Err(Error::Invalid("rollback steps must be at least 1".into()));
    }
    Ok(Self {
      image_name: request.image_name,
      from_layer: request.from_layer,
      steps: request.steps,
    })
  }
}

pub fn image_rollback(request: v1::RollbackImageRequest) -> Result<ImageRollback, Error> {
  request.try_into()
}

impl From<ImageRollbackResult> for v1::RollbackImageResponse {
  fn from(result: ImageRollbackResult) -> Self {
    Self {
      layer: result.layer.unwrap_or_default(),
      base: result.base.unwrap_or_default(),
    }
  }
}

pub fn proto_image_rollback(result: ImageRollbackResult) -> v1::RollbackImageResponse {
  result.into()
}

impl TryFrom<v1::LogImageRequest> for ImageLog {
  type Error = Error;

  fn try_from(request: v1::LogImageRequest) -> Result<Self, Error> {
    if request.image_name.trim().is_empty() {
      return Err(Error::Missing("image name"));
    }
    Ok(Self {
      image_name: request.image_name,
      current_layer: (!request.current_layer.trim().is_empty()).then_some(request.current_layer),
    })
  }
}

pub fn image_log(request: v1::LogImageRequest) -> Result<ImageLog, Error> {
  request.try_into()
}

impl From<CommitHistoryEntry> for v1::CommitHistoryEntry {
  fn from(c: CommitHistoryEntry) -> Self {
    Self {
      layer: c.layer,
      parent: c.parent.unwrap_or_default(),
      base: c.base.unwrap_or_default(),
      commands: c.commands.into_iter().map(Into::into).collect(),
      time_ms: c.time_ms,
      active: c.active,
    }
  }
}

impl From<ImageLogResult> for v1::LogImageResponse {
  fn from(result: ImageLogResult) -> Self {
    Self {
      commits: result.commits.into_iter().map(Into::into).collect(),
    }
  }
}

pub fn proto_image_log(result: ImageLogResult) -> v1::LogImageResponse {
  result.into()
}

impl TryFrom<v1::ExportImageRequest> for ImageExport {
  type Error = Error;

  fn try_from(request: v1::ExportImageRequest) -> Result<Self, Error> {
    if request.image_yaml.trim().is_empty() {
      return Err(Error::Missing("image definition"));
    }
    Ok(Self {
      image_yaml: request.image_yaml,
      driver: request.driver,
      user: request.user.map(image_user),
      commit: request.commit.map(image_commit).transpose()?,
      pinned_features: pinned_features(request.pinned_features)?,
      tag: (!request.tag.trim().is_empty()).then_some(request.tag),
    })
  }
}

pub fn image_export(request: v1::ExportImageRequest) -> Result<ImageExport, Error> {
  request.try_into()
}

impl From<v1::PruneImagesRequest> for crate::control::ImagePrune {
  fn from(request: v1::PruneImagesRequest) -> Self {
    Self {
      all: request.all,
      dry_run: request.dry_run,
      keep: request.keep,
    }
  }
}

impl From<crate::control::ImagePruneResult> for v1::PruneImagesResponse {
  fn from(result: crate::control::ImagePruneResult) -> Self {
    Self {
      layers_removed: result.layers_removed,
      snapshots_removed: result.snapshots_removed,
      bytes_reclaimed: result.bytes_reclaimed,
      removed_images: result.removed_images,
    }
  }
}

#[cfg(test)]
#[path = "domain_test.rs"]
mod tests;
