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
use needy_core::types::{Origin, SessionState};

use crate::control::{
  ExecInput, ExecOutput, ExecSpec, GuestConfig, NewSession, ProcessInfo, Route, SessionInfo, Started, Target, WindowSize,
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

// NOTICE: proto -> `SessionState` lives on the client side
// (`needy-cli`), the only reader of session states on the wire.
pub fn proto_session_state(state: SessionState) -> v1::SessionState {
  match state {
    SessionState::Creating => v1::SessionState::Creating,
    SessionState::Running => v1::SessionState::Running,
    SessionState::Stopping => v1::SessionState::Stopping,
    SessionState::Stopped => v1::SessionState::Stopped,
    SessionState::Finished => v1::SessionState::Finished,
    SessionState::Failed => v1::SessionState::Failed,
  }
}

// ------------------------------------------------------------- sessions

pub fn new_session(request: v1::CreateSessionRequest) -> Result<NewSession, Error> {
  if request.profile_name.is_empty() {
    return Err(Error::Missing("profile"));
  }
  Ok(NewSession {
    profile_yaml: request.profile_yaml,
    profile_dir: request.profile_dir,
    profile_name: request.profile_name,
    group: request.group,
    name: request.name,
    start_actor: request.start_actor,
    actor_command: request.actor_command,
  })
}

pub fn proto_session(session: SessionInfo) -> v1::Session {
  v1::Session {
    session_id: session.id,
    name: session.name,
    group: session.group,
    state: proto_session_state(session.state).into(),
    executors: session
      .executors
      .into_iter()
      .map(|e| v1::ExecutorRef {
        name: e.name,
        runtime: e.runtime,
        instance: e.instance,
      })
      .collect(),
    created_at_ms: session.created_ms,
    profile: session.profile,
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
    session: start.session_id,
    executor: start.executor,
    no_stdin: start.no_stdin,
    search_path_if_missing: start.search_path_if_missing,
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
    session_id: spec.session,
    executor: spec.executor,
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
    }),
    ExecOutput::Stdout(bytes) => Payload::Stdout(bytes),
    ExecOutput::Stderr(bytes) => Payload::Stderr(bytes),
    ExecOutput::Exited(exit) => Payload::Exited(proto_exit(exit)),
    ExecOutput::Error(message) => Payload::Error(message),
    ExecOutput::Ended(reason) => Payload::Ended(v1::ExecEnded { reason }),
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
    session: info.session_id,
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
    session_id: info.session,
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
    session_id: config.session,
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
