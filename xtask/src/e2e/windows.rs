//! A Windows machine as a runner (`--windows-runner SSH`): it builds its
//! own `needy.exe` from this commit with its own toolchain, and joins a
//! cell's control daemon through an ssh reverse tunnel for the length of
//! the cell. Its ssh shell is `cmd.exe`, the default of Windows OpenSSH.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;

use super::proc;

/// A Windows runner the harness built `needy.exe` on.
#[derive(Debug, Clone)]
pub struct Windows {
  pub ssh: String,
  /// The harness's directory there, relative to the ssh user's home (the
  /// session's working directory).
  pub dir: String,
}

fn ssh(host: &str) -> Command {
  let mut command = Command::new("ssh");
  command.args(["-o", "BatchMode=yes", "-o", "ServerAliveInterval=30", host]);
  command
}

/// Ships `HEAD` to the Windows host and builds `needy.exe` there.
pub fn prepare(root: &Path, host: &str, dir: &str) -> Result<Windows> {
  if dir.is_empty() || dir.contains(['/', ':', '.', '&', '|', '"', ' ']) {
    bail!("--windows-dir must be one plain directory name under the Windows home, not {dir:?}");
  }
  eprintln!("e2e: shipping HEAD to the Windows runner and building needy.exe there");
  let mut archive = Command::new("git").current_dir(root).args(["archive", "--format=tar", "HEAD"]).stdout(Stdio::piped()).spawn()?;
  // cmd.exe: `&` sequences, `&&` only on success. bsdtar reads the archive
  // from stdin with `-f -`.
  let script = format!(r"(if exist {dir}\src rmdir /s /q {dir}\src) & mkdir {dir}\src && tar -x -f - -C {dir}\src");
  let mut unpack = ssh(host).arg(script).stdin(archive.stdout.take().context("no archive")?).spawn()?;
  if !archive.wait()?.success() || !unpack.wait()?.success() {
    bail!("cannot ship HEAD to the Windows runner");
  }
  let mut build = ssh(host);
  build.arg(format!(r"cd /d {dir}\src && cargo build --locked --bin needy"));
  let out = proc::run(build, Duration::from_secs(3600))?;
  if !out.ok() {
    bail!("cannot build needy.exe on the Windows runner: {}", out.brief());
  }
  Ok(Windows {
    ssh: host.to_string(),
    dir: dir.to_string(),
  })
}

impl Windows {
  /// Starts `needy.exe daemon --mode runner` named `name` there, joined to
  /// the control daemon's loopback `port` here with `token`, through
  /// `ssh -R`. The runner lives as long as the returned ssh process.
  pub fn start(&self, port: u16, token: &str, name: &str) -> Result<Child> {
    let dir = &self.dir;
    // `set X=Y&&` leaves no trailing space in Y. The state directory is
    // the runner's own, so it never meets another needy there.
    let script = format!(
      r"cd /d {dir} && set NEEDY_STATE_DIR=%CD%\state-{name}&& set NEEDY_E2E_JOIN={token}&& src\target\debug\needy.exe daemon --mode runner --join http://127.0.0.1:{port} --join-token-env NEEDY_E2E_JOIN --name {name} --roles executor"
    );
    let child = Command::new("ssh")
      .args([
        "-o",
        "BatchMode=yes",
        "-o",
        "ExitOnForwardFailure=yes",
        "-R",
      ])
      .arg(format!("{port}:127.0.0.1:{port}"))
      .arg(&self.ssh)
      .arg(script)
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .spawn()
      .context("cannot start the Windows runner")?;
    Ok(child)
  }

  /// Ends every `needy.exe` there whose command line names `name`, and
  /// says how many there were: none once a cell's ssh session closed.
  pub fn leftovers(&self, name: &str) -> Result<u32> {
    let script = format!(
      "$p = @(Get-CimInstance Win32_Process -Filter \"Name='needy.exe'\" | Where-Object {{ $_.CommandLine -like '*--name {name}*' }}); \
       $p | ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }}; \
       Write-Output $p.Count"
    );
    // NOTICE: `-EncodedCommand` takes UTF-16LE in base64, which passes
    // through cmd.exe and ssh without any quoting.
    // Reference: `powershell -?` ("-EncodedCommand ... base-64-encoded string").
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
    let mut command = ssh(&self.ssh);
    command.arg(format!("powershell -NoProfile -NonInteractive -EncodedCommand {encoded}"));
    let out = proc::run(command, Duration::from_secs(60))?;
    if !out.ok() {
      bail!("cannot list processes on the Windows runner: {}", out.brief());
    }
    out.stdout.trim().parse().with_context(|| format!("unexpected process count {:?}", out.stdout.trim()))
  }

  /// Removes the runner's state directory of `name`.
  pub fn clean(&self, name: &str) {
    let mut command = ssh(&self.ssh);
    command.arg(format!(r"if exist {}\state-{name} rmdir /s /q {}\state-{name}", self.dir, self.dir));
    // The state directory is the runner's scratch; a failure leaves it for
    // the next `--clean`.
    let _ = proc::run(command, Duration::from_secs(60));
  }
}
