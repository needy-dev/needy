//! systemd adapter. Manager commands follow systemctl 256:
//! https://www.freedesktop.org/software/systemd/man/256/systemctl.html
use super::{Action, Registration, Scope, State};
use anyhow::{Context as _, Result};
pub(super) struct Manager {
  scope: Scope,
}
impl Manager {
  pub(super) fn new(scope: Scope) -> Self {
    Self { scope }
  }
  fn base(&self) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("systemctl");
    if matches!(self.scope, Scope::User) {
      command.arg("--user");
    }
    command
  }
}
impl super::Manager for Manager {
  fn name(&self) -> &'static str {
    "systemd"
  }
  fn definition(&self) -> Result<std::path::PathBuf> {
    Ok(match self.scope {
      Scope::System => "/etc/systemd/system/needy.service".into(),
      Scope::User => {
        let base = std::env::var_os("XDG_CONFIG_HOME")
          .map(std::path::PathBuf::from)
          .or_else(|| std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".config")))
          .context("cannot find user systemd directory")?;
        base.join("systemd/user/needy.service")
      }
    })
  }
  fn render(&self, registration: &Registration) -> Result<String> {
    let user = if matches!(self.scope, Scope::System) {
      format!("User={}\n", registration.account)
    } else {
      String::new()
    };
    let engine =
      registration.engine_host.as_ref().map(|host| format!("Environment={}\n", quote(&format!("DOCKER_HOST={host}")))).unwrap_or_default();
    let directories = [
      ("NEEDY_STATE_DIR", &registration.state),
      ("NEEDY_DATA_DIR", &registration.data),
      ("NEEDY_RUNTIME_DIR", &registration.runtime),
      ("NEEDY_CONFIG_DIR", &registration.configuration),
    ]
    .iter()
    .map(|(key, path)| format!("Environment={}\n", quote(&format!("{key}={}", path.display()))))
    .collect::<String>();
    Ok(format!(
      "# {}\n[Unit]\nDescription=Needy control daemon\n[Service]\nType=simple\n{}ExecStart={} --config {} daemon serve\nWorkingDirectory=/\nEnvironment={}\n{}{directories}Restart=on-failure\nTimeoutStopSec=65s\nKillSignal=SIGTERM\n[Install]\nWantedBy={}\n",
      super::MARKER,
      user,
      quote(&registration.executable.to_string_lossy()),
      quote(&registration.snapshot.to_string_lossy()),
      quote(&format!("PATH={}", registration.path)),
      engine,
      if matches!(self.scope, Scope::User) {
        "default.target"
      } else {
        "multi-user.target"
      }
    ))
  }
  fn command(&self, action: Action, _: &Registration) -> tokio::process::Command {
    let mut command = self.base();
    command.arg(match action {
      Action::Enable => "enable",
      Action::Disable => "disable",
      Action::Start | Action::Kickstart => "start",
      Action::Stop => "stop",
      Action::Reload => "daemon-reload",
    });
    if !matches!(action, Action::Reload) {
      command.arg("needy.service");
    }
    command
  }
  fn inspect(&self, _: &Registration) -> tokio::process::Command {
    let mut command = self.base();
    command.args([
      "show",
      "needy.service",
      "--property=LoadState,ActiveState,SubState,MainPID",
      "--no-pager",
    ]);
    command
  }
  fn inspect_binding(&self, _: &Registration) -> tokio::process::Command {
    let mut command = self.base();
    command.args([
      "show",
      "needy.service",
      "--property=FragmentPath",
      "--no-pager",
    ]);
    command
  }
  fn owns_binding(&self, registration: &Registration, output: &str) -> bool {
    // systemctl(256) FragmentPath identifies the unit actually loaded by the
    // manager, including conflicting transient or overridden registrations.
    output.lines().find_map(|line| line.strip_prefix("FragmentPath=")) == registration.definition.to_str()
  }
  fn parse_state(&self, output: &str) -> Result<State> {
    let value = |name: &str| output.lines().find_map(|line| line.strip_prefix(name)).unwrap_or("");
    let pid = value("MainPID=").parse::<u32>().ok().filter(|pid| *pid > 0);
    let state = match value("ActiveState=") {
      "active" => "running",
      "activating" => "starting",
      "failed" => "failed",
      "inactive" | "deactivating" => "stopped",
      _ => "unknown",
    };
    Ok(State {
      state: state.into(),
      pid,
      loaded: value("LoadState=") != "not-found",
    })
  }
  fn logs(&self, _: &Registration, follow: bool, tail: usize) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("journalctl");
    if matches!(self.scope, Scope::User) {
      command.arg("--user");
    }
    command.args(["--unit=needy.service", "--no-pager", "--lines"]).arg(tail.to_string());
    if follow {
      command.arg("--follow");
    }
    command
  }
}
// systemd.exec(256): double quotes escape values; %% prevents specifier expansion.
fn quote(value: &str) -> String {
  format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%").replace('\n', "\\n").replace('\r', "\\r"))
}
