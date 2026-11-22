//! Proto <-> domain mappings for `ProcessService`.

use needy_api_proto::v1;
use needy_core::event::Event;

use crate::control::{ExecInput, ExecOutput, ExecSpec, ProcessInfo, Started};

use super::{Error, exit, origin, proto_diagnostic, proto_exit, proto_origin};

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
