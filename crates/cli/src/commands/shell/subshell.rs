//! The subshell form: starts the user's shell with startup files that run
//! the user's own startup files first and the activation script last, waits
//! for it, and returns its exit code.

use std::os::unix::process::ExitStatusExt;
use std::path::Path;

use anyhow::{Context as _, Result};
use tokio::signal::unix::{SignalKind, signal};

use super::{SandboxedCommand, Shell, script};

pub(super) async fn run(program: &Path, shell: Shell, sandboxed: &SandboxedCommand) -> Result<i32> {
  // The startup files delete this private directory when the shell reads
  // them, so the user never sees it; dropping it after the shell exits
  // covers a shell that never read them.
  let dir = tempfile::Builder::new().prefix("needy-shell-").tempdir().context("create the shell's startup directory")?;
  let path = dir.path().to_str().context("the temporary directory's path is not valid UTF-8")?;
  let mut command = tokio::process::Command::new(program);
  match shell {
    Shell::Zsh => {
      std::fs::write(dir.path().join(".zshenv"), script::zshenv(path, sandboxed)).context("write the shell's startup file")?;
      if let Some(user) = std::env::var_os("ZDOTDIR") {
        command.env("NEEDY_SHELL_ZDOTDIR", user);
      }
      command.env("ZDOTDIR", dir.path()).arg("-i");
    }
    Shell::Bash => {
      let rc = dir.path().join("bashrc");
      std::fs::write(&rc, script::bashrc(path, sandboxed)).context("write the shell's startup file")?;
      command.arg("--rcfile").arg(&rc).arg("-i");
    }
  }
  // NOTICE: the terminal sends Ctrl-C and Ctrl-\ to its foreground process
  // group, which can include needy while the shell starts. Handling them
  // keeps needy waiting for the shell, as `system(3)` ignores them. A
  // handler, unlike `SIG_IGN`, is reset when the shell is executed, so the
  // shell keeps its default dispositions.
  let _interrupt = signal(SignalKind::interrupt())?;
  let _quit = signal(SignalKind::quit())?;
  let status = command.status().await.with_context(|| format!("start {}", program.display()))?;
  drop(dir);
  // A shell that a signal ended exits as a shell reports it: 128 + the signal.
  Ok(status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or_default()))
}
