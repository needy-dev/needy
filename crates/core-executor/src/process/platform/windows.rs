//! Windows processes: no POSIX signals or process groups; any signal ends the
//! process tree.

use needy_common::process::Exit;

/// Windows has no process groups; `signal_group` ends the tree instead.
pub fn own_group(_command: &mut tokio::process::Command) {}

/// Windows has no inherited signal dispositions to reset.
pub fn default_signals(_command: &mut tokio::process::Command) {}

pub fn exit_from_std(status: std::process::ExitStatus) -> Exit {
  Exit {
    code: status.code(),
    signal: String::new(),
  }
}

pub fn wait_terminal(child: &mut Box<dyn portable_pty::Child + Send + Sync>, pid: i64) -> Exit {
  match child.wait() {
    Ok(status) => Exit {
      code: Some(status.exit_code() as i32),
      signal: String::new(),
    },
    Err(e) => {
      tracing::warn!("process {pid}: wait: {e}");
      super::proc::lost()
    }
  }
}

/// Ends the process tree of `pid`, whatever the signal.
// TODO(windows): job objects would end the tree without spawning `taskkill`
// and without missing children that left it; deferred until the Windows
// executor gets terminals (see the closed-loop results).
pub fn signal_group(pid: i64, name: &str) {
  if pid <= 0 {
    return;
  }
  match std::process::Command::new("taskkill").args(["/T", "/F", "/PID", &pid.to_string()]).output() {
    Ok(output) if !output.status.success() => {
      tracing::debug!("process {pid}: {name}: taskkill: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(_) => {}
    Err(e) => tracing::warn!("process {pid}: {name}: cannot run taskkill: {e}"),
  }
}
