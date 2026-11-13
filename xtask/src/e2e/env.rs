//! One cell's world: its own directories, its own control daemon (started
//! and stopped only by the harness, with its PID recorded), and the `needy`
//! commands that talk to it.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use nix::sys::signal::Signal;

use super::host::{EXECUTOR_IMAGE, Host};
use super::proc::{self, Output};
use super::runs::tail as runs_tail;
use super::term::Term;

/// Every daemon the harness started and has not seen exit, so an
/// interrupted harness ends them all (see [`kill_all`]).
static DAEMONS: Mutex<Vec<i32>> = Mutex::new(Vec::new());

/// Ends every daemon the harness started: an interrupted run leaves none.
pub fn kill_all() {
  let pids: Vec<i32> = DAEMONS.lock().map(|d| d.clone()).unwrap_or_default();
  for pid in &pids {
    proc::signal(*pid, Signal::SIGTERM);
  }
  let deadline = Instant::now() + Duration::from_secs(10);
  while pids.iter().any(|p| proc::alive(*p)) && Instant::now() < deadline {
    std::thread::sleep(Duration::from_millis(100));
  }
  for pid in pids {
    proc::signal(pid, Signal::SIGKILL);
  }
}

/// What every cell shares: the harness's inputs, read only.
pub struct Ctx {
  /// The `needy` under test.
  pub needy: PathBuf,
  pub host: Host,
  /// Keep each cell's directory instead of removing it.
  pub keep: bool,
  /// Where a failed cell's logs, profiles and consoles are kept.
  pub results: PathBuf,
  /// How long one cell may take.
  pub timeout: Duration,
  /// The Windows runner (`--windows-runner`), or why it is not there.
  pub windows: Option<Result<super::windows::Windows, String>>,
  /// The local settings of the transports that need outside machines.
  pub transports: super::transport::Settings,
}

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A cell's directory and daemon.
pub struct CellEnv<'a> {
  pub ctx: &'a Ctx,
  /// The cell's root. Short on purpose: Unix socket paths under it must
  /// fit `sun_path` (104 bytes on macOS, 108 on Linux).
  pub dir: PathBuf,
  daemon: Option<Child>,
  /// The loopback TCP port the daemon also listens on, for a runner that
  /// joins through a tunnel.
  pub tcp: Option<u16>,
  /// Runners the cell started: a local `runner serve`, or the ssh
  /// session a remote one lives in.
  runners: Vec<Child>,
  /// The runs the cell created, for the leftover checks.
  pub runs: Vec<String>,
  /// Profiles written so far, for the artifacts.
  profiles: u32,
}

impl<'a> CellEnv<'a> {
  pub fn new(ctx: &'a Ctx) -> Result<Self> {
    // NOTICE: `/tmp`, not `std::env::temp_dir()`: macOS's per-user temp
    // directory (`/var/folders/...`) is about 50 bytes before a run's own
    // directories and sockets begin, too long for `sun_path`.
    let base = Path::new("/tmp");
    let seed = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    for _ in 0..100 {
      let n = NEXT.fetch_add(1, Ordering::Relaxed);
      let dir = base.join(format!("ne2e-{:x}{:x}", std::process::id() & 0xfff, (seed ^ n.wrapping_mul(2654435761)) & 0xfffff));
      match std::fs::create_dir(&dir) {
        Ok(()) => {
          use std::os::unix::fs::PermissionsExt;
          // Open, so the sockets' privacy comes from the daemon's own
          // directories, which the socket checks look at.
          std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755))?;
          return Ok(Self {
            ctx,
            dir,
            daemon: None,
            tcp: None,
            runners: Vec::new(),
            runs: Vec::new(),
            profiles: 0,
          });
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
        Err(e) => return Err(e).with_context(|| format!("cannot create {}", dir.display())),
      }
    }
    bail!("cannot find a free cell directory under {}", base.display())
  }

  /// Process identity from the child handle the harness itself started.
  pub fn daemon_pid(&self) -> Option<u32> {
    self.daemon.as_ref().map(Child::id)
  }

  pub fn state(&self) -> PathBuf {
    self.dir.join("s")
  }

  pub fn runtime(&self) -> PathBuf {
    self.dir.join("r")
  }

  pub fn path(&self, relative: &str) -> PathBuf {
    self.dir.join(relative)
  }

  /// A directory under the cell, created with `mode`.
  pub fn mkdir(&self, relative: &str, mode: u32) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let dir = self.dir.join(relative);
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode))?;
    Ok(dir)
  }

  /// A file under the cell with `content` and `mode`, parents created.
  pub fn file(&self, relative: &str, content: &str, mode: u32) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = self.dir.join(relative);
    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, content).with_context(|| format!("cannot write {}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))?;
    Ok(path)
  }

  /// The environment of every needy the cell starts: its own directories,
  /// none of the harness user's.
  fn env(&self) -> Vec<(String, String)> {
    let mut env = vec![
      ("NEEDY_STATE_DIR".to_string(), self.state().display().to_string()),
      ("NEEDY_RUNTIME_DIR".to_string(), self.runtime().display().to_string()),
      ("NEEDY_CONFIG_DIR".to_string(), self.dir.join("c").display().to_string()),
      ("NEEDY_DATA_DIR".to_string(), self.dir.join("d").display().to_string()),
      // Logs are files the harness reads back, not a terminal.
      ("NO_COLOR".to_string(), "1".to_string()),
    ];
    if let Some(host) = &self.ctx.host.docker_host {
      env.push(("DOCKER_HOST".to_string(), host.clone()));
    }
    env
  }

  fn command(&self, args: &[&str]) -> Command {
    let mut command = Command::new(&self.ctx.needy);
    command.args(args).current_dir(&self.dir);
    // A caller's own run, token or endpoint must not leak into a cell.
    for var in [
      "NEEDY_RUN",
      "NEEDY_TOKEN",
      "NEEDY_ENDPOINT",
      "NEEDY_EXECUTION_CONTEXT",
    ] {
      command.env_remove(var);
    }
    command.envs(self.env());
    command
  }

  /// `needy ARGS` against this cell's daemon.
  pub fn needy(&self, args: &[&str], timeout: Duration) -> Result<Output> {
    proc::run(self.command(args), timeout)
  }

  /// The environment of a `needy` the cell starts on a pseudo-terminal of
  /// the TUI harness, as `KEY=VALUE`: the cell's, and no run of the
  /// harness's caller.
  pub fn cell_env(&self) -> Vec<String> {
    let mut env: Vec<String> = self.env().into_iter().map(|(k, v)| format!("{k}={v}")).collect();
    env.extend(
      [
        "NEEDY_RUN=",
        "NEEDY_TOKEN=",
        "NEEDY_ENDPOINT=",
        "NEEDY_EXECUTION_CONTEXT=",
      ]
      .map(str::to_string),
    );
    env
  }

  /// `needy ARGS` on a pseudo-terminal.
  pub fn term(&self, args: &[&str]) -> Result<Term> {
    let mut env = self.env();
    env.extend(
      [
        "NEEDY_RUN",
        "NEEDY_TOKEN",
        "NEEDY_ENDPOINT",
        "NEEDY_EXECUTION_CONTEXT",
      ]
      .map(|name| (name.into(), String::new())),
    );
    Term::spawn(&self.ctx.needy, args, &env, &self.dir)
  }

  /// Starts this cell's control daemon as the harness's own user and waits
  /// until it answers.
  pub fn start_daemon(&mut self) -> Result<()> {
    // The VM images the harness built from container images (the TUI
    // cells' agents image) are where the daemon looks for a profile's
    // `actor.image`: its data directory's `images`.
    let images = self.dir.join("d").join("images");
    if let Some(built) = &self.ctx.host.images
      && built.exists()
      && !images.exists()
    {
      std::fs::create_dir_all(self.dir.join("d"))?;
      std::os::unix::fs::symlink(built, &images).with_context(|| format!("cannot link {}", images.display()))?;
    }
    let log = File::options().create(true).append(true).open(self.dir.join("daemon.log"))?;
    let mut args = vec![
      "daemon".to_string(),
      "serve".into(),
      "--local-runner".into(),
      "e2e".into(),
      "-v".into(),
    ];
    if let Some(port) = self.tcp {
      args.extend(["--listen".into(), format!("tcp://127.0.0.1:{port}")]);
    }
    if let Ok(guest) = &self.ctx.host.guest {
      args.extend(["--guest-needy".into(), guest.display().to_string()]);
    }
    if self.ctx.host.container_executor.is_ok() {
      args.extend(["--executor-image".into(), EXECUTOR_IMAGE.into()]);
    }
    if let Some(image) = &self.ctx.host.vm_image {
      args.extend(["--actor-image".into(), image.display().to_string()]);
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut command = self.command(&args);
    command.stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
    let child = command.spawn().context("cannot start the control daemon")?;
    let pid = child.id() as i32;
    DAEMONS.lock().map(|mut d| d.push(pid)).ok();
    std::fs::write(self.dir.join("daemon.pid"), pid.to_string())?;
    self.daemon = Some(child);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
      let out = self.needy(&["run", "ls", "-o", "json"], Duration::from_secs(10))?;
      if out.ok() {
        return Ok(());
      }
      if let Some(child) = &mut self.daemon
        && let Some(status) = child.try_wait()?
      {
        self.forget_daemon();
        let log = std::fs::read_to_string(self.dir.join("daemon.log")).unwrap_or_default();
        bail!("the control daemon exited at start ({status}); its log ends: {}", runs_tail(&log));
      }
      if Instant::now() > deadline {
        bail!("the control daemon did not answer within 30 s: {}", out.brief());
      }
      std::thread::sleep(Duration::from_millis(100));
    }
  }

  fn forget_daemon(&mut self) {
    if let Some(child) = self.daemon.take() {
      let pid = child.id() as i32;
      DAEMONS.lock().map(|mut d| d.retain(|p| *p != pid)).ok();
    }
  }

  /// Kills the daemon with SIGKILL, as a crash would.
  pub fn kill_daemon(&mut self) -> Result<()> {
    let Some(mut child) = self.daemon.take() else {
      bail!("no daemon runs");
    };
    child.kill().context("cannot kill the control daemon")?;
    child.wait()?;
    let pid = child.id() as i32;
    DAEMONS.lock().map(|mut d| d.retain(|p| *p != pid)).ok();
    Ok(())
  }

  /// Enrolls a runner on this host through `join`, then serves its saved
  /// enrollment with directories of its own and the transport environment.
  pub fn start_local_runner(&mut self, token: &str, name: &str, join: &str, env: &[(String, String)]) -> Result<()> {
    let log = File::options().create(true).append(true).open(self.dir.join("runner.log"))?;
    let code = self.file("runner-code", token, 0o600)?;
    let mut args = vec![
      "runner".to_string(),
      "join".into(),
      name.into(),
      "--code-file".into(),
      code.display().to_string(),
      "--roles".into(),
      "executor".into(),
    ];
    if let Some(command) = join.strip_prefix("exec:") {
      args.push("--".into());
      args.extend(shell_words::split(command).context("invalid runner connector argv")?);
    } else {
      args.extend(["--address".into(), join.into()]);
    }
    let configure = |command: &mut Command| -> Result<()> {
      command
        .env("NEEDY_STATE_DIR", self.dir.join("rs"))
        .env("NEEDY_RUNTIME_DIR", self.dir.join("rr"))
        .env("NEEDY_CONFIG_DIR", self.dir.join("rc"))
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log.try_clone()?);
      Ok(())
    };
    let mut enroll = self.command(&args.iter().map(String::as_str).collect::<Vec<_>>());
    configure(&mut enroll)?;
    let enrolled = proc::run(enroll, Duration::from_secs(60));
    std::fs::remove_file(code).context("cannot remove the consumed runner invitation file")?;
    let enrolled = enrolled?;
    if !enrolled.ok() {
      let log = std::fs::read_to_string(self.dir.join("runner.log")).unwrap_or_default();
      bail!("cannot enroll the local runner: {}; log: {}", enrolled.brief(), runs_tail(&log));
    }
    let mut command = self.command(&["runner", "serve", "-v"]);
    configure(&mut command)?;
    let child = command.spawn().context("cannot start the local runner")?;
    DAEMONS.lock().map(|mut d| d.push(child.id() as i32)).ok();
    self.runners.push(child);
    Ok(())
  }

  /// Keeps `child` (a remote runner's ssh session) with the cell, so it
  /// ends with it.
  pub fn add_runner(&mut self, child: Child) {
    DAEMONS.lock().map(|mut d| d.push(child.id() as i32)).ok();
    self.runners.push(child);
  }

  /// Stops every runner the cell started: SIGTERM, then SIGKILL after 10 s.
  pub fn stop_runners(&mut self) {
    for mut child in self.runners.drain(..) {
      let pid = child.id() as i32;
      proc::signal(pid, Signal::SIGTERM);
      let deadline = Instant::now() + Duration::from_secs(10);
      while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
      }
      // Gone already, or killed now: either way it ends with the cell.
      let _ = child.kill();
      let _ = child.wait();
      DAEMONS.lock().map(|mut d| d.retain(|p| *p != pid)).ok();
    }
  }

  /// Stops the daemon with SIGTERM and waits for it. A daemon that does
  /// not exit within 30 s is killed, and that is the error. The cell's
  /// runners stop first.
  pub fn stop_daemon(&mut self) -> Result<(), String> {
    self.stop_runners();
    let Some(mut child) = self.daemon.take() else {
      return Ok(());
    };
    let pid = child.id() as i32;
    proc::signal(pid, Signal::SIGTERM);
    let deadline = Instant::now() + Duration::from_secs(30);
    let result = loop {
      match child.try_wait() {
        Ok(Some(status)) if status.success() => break Ok(()),
        Ok(Some(status)) => break Err(format!("the control daemon exited with {status} on SIGTERM")),
        Ok(None) if Instant::now() > deadline => {
          // Killed is what the caller wanted anyway; the error says why.
          let _ = child.kill();
          let _ = child.wait();
          break Err("the control daemon did not exit within 30 s of SIGTERM".to_string());
        }
        Ok(None) => std::thread::sleep(Duration::from_millis(100)),
        Err(e) => break Err(format!("cannot wait for the control daemon: {e}")),
      }
    };
    DAEMONS.lock().map(|mut d| d.retain(|p| *p != pid)).ok();
    result
  }

  /// The daemon's last warnings and errors, for a failure's detail.
  pub fn daemon_log_tail(&self) -> String {
    let log = std::fs::read_to_string(self.dir.join("daemon.log")).unwrap_or_default();
    let lines: Vec<&str> = log.lines().filter(|l| l.contains("WARN") || l.contains("ERROR")).collect();
    lines[lines.len().saturating_sub(4)..].join(" | ")
  }

  /// Writes `profile` (JSON, which is YAML) under the cell and returns its
  /// path.
  pub fn profile(&mut self, profile: &serde_json::Value) -> Result<PathBuf> {
    self.profiles += 1;
    let path = self.dir.join(format!("profile-{}.yaml", self.profiles));
    std::fs::write(&path, serde_json::to_string_pretty(profile)?)?;
    Ok(path)
  }
}

impl Drop for CellEnv<'_> {
  fn drop(&mut self) {
    self.stop_runners();
    if let Some(mut child) = self.daemon.take() {
      let pid = child.id() as i32;
      // The cell is over; whatever state the daemon was in, it goes.
      let _ = child.kill();
      let _ = child.wait();
      DAEMONS.lock().map(|mut d| d.retain(|p| *p != pid)).ok();
    }
  }
}
