//! launchd adapter. Property-list keys follow Apple's archived launchd guide:
//! https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html
use super::{Action, Registration, Scope, State};
use anyhow::{Context as _, Result};
const LABEL: &str = "dev.needy.control";
pub(super) struct Manager {
  scope: Scope,
}
impl Manager {
  pub(super) fn new(scope: Scope) -> Self {
    Self { scope }
  }
  fn domain(&self, registration: &Registration) -> String {
    match self.scope {
      Scope::User => format!("gui/{}", registration.uid),
      Scope::System => "system".into(),
    }
  }
}
impl super::Manager for Manager {
  fn name(&self) -> &'static str {
    "launchd"
  }
  fn definition(&self) -> Result<std::path::PathBuf> {
    Ok(match self.scope {
      Scope::System => std::path::PathBuf::from("/Library/LaunchDaemons").join(format!("{LABEL}.plist")),
      Scope::User => std::path::PathBuf::from(std::env::var_os("HOME").context("cannot find user LaunchAgents directory")?)
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")),
    })
  }
  fn render(&self, registration: &Registration) -> Result<String> {
    let account = if matches!(self.scope, Scope::System) {
      format!("<key>UserName</key><string>{}</string>", xml(&registration.account))
    } else {
      String::new()
    };
    let engine =
      registration.engine_host.as_ref().map(|host| format!("<key>DOCKER_HOST</key><string>{}</string>", xml(host))).unwrap_or_default();
    let directories = [
      ("NEEDY_STATE_DIR", &registration.state),
      ("NEEDY_DATA_DIR", &registration.data),
      ("NEEDY_RUNTIME_DIR", &registration.runtime),
      ("NEEDY_CONFIG_DIR", &registration.configuration),
    ]
    .iter()
    .map(|(key, path)| format!("<key>{key}</key><string>{}</string>", xml(&path.to_string_lossy())))
    .collect::<String>();
    Ok(format!(
      "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!-- {} -->\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>{LABEL}</string><key>ProgramArguments</key><array><string>{}</string><string>--config</string><string>{}</string><string>daemon</string><string>serve</string></array>{account}<key>WorkingDirectory</key><string>/</string><key>EnvironmentVariables</key><dict><key>PATH</key><string>{}</string>{engine}{directories}</dict><key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict><key>ExitTimeOut</key><integer>65</integer><key>StandardOutPath</key><string>{}</string><key>StandardErrorPath</key><string>{}</string></dict></plist>\n",
      super::MARKER,
      xml(&registration.executable.to_string_lossy()),
      xml(&registration.snapshot.to_string_lossy()),
      xml(&registration.path),
      xml(&registration.log.to_string_lossy()),
      xml(&registration.log.to_string_lossy())
    ))
  }
  fn command(&self, action: Action, registration: &Registration) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("launchctl");
    let target = format!("{}/{LABEL}", self.domain(registration));
    match action {
      Action::Enable => {
        command.args(["enable", &target]);
      }
      Action::Disable => {
        command.args(["disable", &target]);
      }
      Action::Start => {
        command.arg("bootstrap").arg(self.domain(registration)).arg(&registration.definition);
      }
      Action::Stop => {
        command.args(["bootout", &target]);
      }
      Action::Kickstart => {
        command.args(["kickstart", &target]);
      }
      // launchd reads the plist at bootstrap and needs no manager reload.
      Action::Reload => {
        command.args(["print", &self.domain(registration)]);
      }
    }
    command
  }
  fn inspect(&self, registration: &Registration) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("launchctl");
    // Listing the domain succeeds even while the owned job is booted out.
    command.args(["print", &self.domain(registration)]);
    command
  }
  fn inspect_binding(&self, registration: &Registration) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("launchctl");
    command.args(["print", &format!("{}/{LABEL}", self.domain(registration))]);
    command
  }
  fn owns_binding(&self, registration: &Registration, output: &str) -> bool {
    // `launchctl print DOMAIN/LABEL` names the loaded plist and program.
    // Check both: a stale receipt alone cannot authorize bootout of a job
    // another installation loaded under the canonical label.
    let field = |key: &str| output.lines().find_map(|line| line.trim().strip_prefix(key));
    field("path = ") == registration.definition.to_str() && field("program = ") == registration.executable.to_str()
  }
  fn parse_state(&self, output: &str) -> Result<State> {
    // `launchctl print DOMAIN`'s service table has PID, status, label.
    let row = output.lines().find(|line| line.split_whitespace().last() == Some(LABEL));
    let fields: Vec<_> = row.unwrap_or("").split_whitespace().collect();
    let pid = fields.first().and_then(|value| value.parse::<u32>().ok()).filter(|pid| *pid > 0);
    let failed = fields.get(1).is_some_and(|value| *value != "0" && *value != "-");
    Ok(State {
      state: if pid.is_some() {
        "running"
      } else if failed {
        "failed"
      } else {
        "stopped"
      }
      .into(),
      pid,
      loaded: row.is_some(),
    })
  }
  fn logs(&self, registration: &Registration, follow: bool, tail: usize) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("tail");
    command.arg("-n").arg(tail.to_string());
    if follow {
      command.arg("-f");
    }
    command.arg(&registration.log);
    command
  }
}
fn xml(value: &str) -> String {
  value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}
