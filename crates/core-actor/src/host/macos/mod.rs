//! The `macos-native` actor runtime, a proof of concept: no VM. The
//! driving program is a process on this Mac under a Seatbelt profile (the
//! [`fence`]), with a directory of shims first and alone on its `PATH`.
//!
//! The runtime starts the actor's init, this binary started again as
//! `needy __needy-macos-init DIR` (see `crate::native`), next to the daemon
//! and outside the fence. Init asks the daemon for the run's configuration
//! on the guest socket, as a container's init does, makes the shims and the
//! fence, forwards a loopback port to the gateway socket, and starts the
//! driving program with `sandbox-exec`. A shim routes a command the run
//! routes, as a container's shim does; for any other command it asks init,
//! which starts the real program under the same fence with that one program
//! allowed (a kept command). Everything else that would go around a shim,
//! an absolute path, a program written in the workspace, a script, fails
//! with `EPERM`, and the kernel's log of each denial becomes an
//! `actor.deny` event.
//!
//! What it does not do is in
//! `docs/ai/references/architecture/2026-11-12-macos-native-actor-poc.md`:
//! no identity, no workspace mounts, the host's processes and users are
//! visible, and code an allowed program runs in its own process is not
//! routed.

pub mod fence;

use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;

use needy_config::recipe::IdentityField;
use needy_core::recipe::CompiledRecipe;

use super::container::{GATEWAY_SOCKET, GUEST_SOCKET, PROCESS_SOCKET, channels_dir};
use super::runtime::{ActorRuntime, Channels, Endpoint, Error, Gap, RunningActor, Setup, Start, Terminal};

/// The argument that makes the needy binary the actor's init:
/// `needy __needy-macos-init DIR`.
pub const INIT: &str = "__needy-macos-init";
/// The argument that makes the needy binary the start gate of a driving
/// program on a terminal: `needy __needy-macos-gate SOCKET -- PROGRAM…`.
pub const GATE: &str = "__needy-macos-gate";
/// Where `sandbox-exec` is; the only one trusted, as Codex trusts only it.
// Source: https://github.com/openai/codex/blob/392ced7d3a9160fd35d40c606359869215f50522/codex-rs/sandboxing/src/seatbelt.rs#L57-L61
pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
/// The file that marks a directory of shims; a needy binary started from a
/// link in such a directory is a shim.
pub const SHIM_MARKER: &str = ".needy-shims";
/// The kept-command service init serves, in the channels directory.
pub const KEPT_SOCKET: &str = "kept.sock";

/// Where one native actor keeps its files: its directory `dir` (the run's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
  pub dir: PathBuf,
}

impl Layout {
  pub fn new(dir: &Path) -> Self {
    Self {
      dir: dir.to_path_buf(),
    }
  }

  /// The layout of the actor whose shim is `exe`, a link in its shims
  /// directory, when `exe` is one.
  pub fn of_shim(exe: &Path) -> Option<Self> {
    let shims = exe.parent()?;
    if !shims.join(SHIM_MARKER).exists() {
      return None;
    }
    Some(Self::new(shims.parent()?.parent()?))
  }

  pub fn guest(&self) -> PathBuf {
    channels_dir(&self.dir).join(GUEST_SOCKET)
  }

  pub fn gateway(&self) -> PathBuf {
    channels_dir(&self.dir).join(GATEWAY_SOCKET)
  }

  pub fn process(&self) -> PathBuf {
    channels_dir(&self.dir).join(PROCESS_SOCKET)
  }

  pub fn kept(&self) -> PathBuf {
    channels_dir(&self.dir).join(KEPT_SOCKET)
  }

  /// The actor's own files, apart from its channels.
  fn actor(&self) -> PathBuf {
    self.dir.join("actor")
  }

  /// The driving program's home directory.
  pub fn home(&self) -> PathBuf {
    self.actor().join("home")
  }

  /// The driving program's `TMPDIR`.
  pub fn tmp(&self) -> PathBuf {
    self.actor().join("tmp")
  }

  pub fn shims(&self) -> PathBuf {
    self.actor().join("shims")
  }

  /// The fence's profile.
  pub fn fence(&self) -> PathBuf {
    self.actor().join("fence.sb")
  }

  /// The route list, which shims read: the format of a container's
  /// `/run/needy/route` (a user ID line, then the names).
  pub fn route(&self) -> PathBuf {
    self.actor().join("route")
  }

  /// The driving program's process group, which a forced stop kills.
  pub fn driver_pid(&self) -> PathBuf {
    self.actor().join("driver.pid")
  }
}

/// The `macos-native` runtime.
pub struct MacosNative;

#[async_trait::async_trait]
impl ActorRuntime for MacosNative {
  fn name(&self) -> &'static str {
    "macos-native"
  }

  fn label(&self) -> String {
    "macos-native (seatbelt, no VM)".into()
  }

  async fn check_host(&self) -> Result<(), Error> {
    if !Path::new(SANDBOX_EXEC).exists() {
      return Err(Error::Unsupported(format!("{SANDBOX_EXEC} is missing; a macos-native actor needs it")));
    }
    Ok(())
  }

  fn channels(&self, dir: &Path) -> Channels {
    let layout = Layout::new(dir);
    Channels {
      sockets: vec![
        layout.guest(),
        layout.gateway(),
        layout.process(),
        layout.kept(),
      ],
      guest: Endpoint::Unix(layout.guest()),
      gateway: Endpoint::Unix(layout.gateway()),
      terminal: Terminal::Unix(layout.process()),
      // Init finds its channels in its directory, as a container's init
      // finds them at fixed paths.
      gateway_port: 0,
      process_port: 0,
      // The driving program runs as the daemon's user.
      guest_for_every_user: false,
    }
  }

  fn setup(&self) -> Setup {
    // The daemon then sends init no mounts, hostname or sensors: a native
    // actor has none of them.
    Setup::Host
  }

  async fn start(&self, start: &Start<'_>) -> Result<Box<dyn RunningActor>, Error> {
    // TODO(macos-native): workspace mounts from executors need FUSE on the
    // Mac (macFUSE or FSKit); the proof of concept uses a local workspace
    // that an executor shares (its `workspace:` and `path_map`).
    if let Some(mount) = start.mounts.first() {
      return Err(Error::Unsupported(format!(
        "a macos-native actor cannot mount {} from executor {}; give the actor a local workspace (`actor.cwd`) and the executor the same directory",
        mount.target, mount.executor
      )));
    }
    let exe = std::env::current_exe().map_err(Error::Spawn)?;
    let log = super::runtime::log(start.dir);
    let console = std::fs::File::create(&log).map_err(|source| Error::Write {
      path: log.clone(),
      source,
    })?;
    let stderr = console.try_clone().map_err(|source| Error::Write { path: log, source })?;
    let mut command = tokio::process::Command::new(exe);
    command.arg(INIT).arg(start.dir).stdin(std::process::Stdio::null()).stdout(console).stderr(stderr).process_group(0);
    let child = command.spawn().map_err(Error::Spawn)?;
    let pid = child.id().unwrap_or_default();
    let vcpus = std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1);
    Ok(Box::new(Native {
      child,
      pid,
      vcpus,
      layout: Layout::new(start.dir),
    }))
  }

  fn identity_gaps(&self, recipe: Option<&CompiledRecipe>) -> Vec<Gap> {
    let _ = recipe;
    let mut gaps: Vec<Gap> = [
      IdentityField::Hostname,
      IdentityField::Cpuinfo,
      IdentityField::Cpuid,
      IdentityField::Dmi,
      IdentityField::Pci,
      IdentityField::Hwmon,
      IdentityField::Kernel,
      IdentityField::Uptime,
    ]
    .into_iter()
    .map(|field| Gap::of(field, "the driving program sees this Mac's own"))
    .collect();
    gaps.push(Gap {
      field: None,
      message: "experimental: a process sandbox on this Mac's kernel, not a VM; the host's processes, users and files outside the fence's hidden paths are visible".into(),
    });
    gaps
  }
}

/// A started native actor: its init process.
struct Native {
  child: tokio::process::Child,
  pid: u32,
  vcpus: u32,
  layout: Layout,
}

#[async_trait::async_trait]
impl RunningActor for Native {
  /// Init's process ID.
  fn pid(&self) -> u32 {
    self.pid
  }

  /// This Mac's: a native actor has no size of its own.
  fn vcpus(&self) -> u32 {
    self.vcpus
  }

  // TODO(macos-native): no memory limit; Seatbelt has none, and a resource
  // limit (`setrlimit`) is per process, not per actor.
  fn mem_mib(&self) -> u32 {
    0
  }

  async fn wait(&mut self) -> std::io::Result<ExitStatus> {
    self.child.wait().await
  }

  /// `SIGTERM` to init, which ends the driving program and the kept
  /// commands; then `SIGKILL` to init and to the driving program's process
  /// group once `grace` has passed.
  async fn stop(&mut self, grace: Duration) -> std::io::Result<ExitStatus> {
    use nix::sys::signal::{Signal, kill, killpg};
    use nix::unistd::Pid;
    if let Some(status) = self.child.try_wait()? {
      return Ok(status);
    }
    match kill(Pid::from_raw(self.pid as i32), Signal::SIGTERM) {
      Ok(()) => {
        if let Ok(waited) = tokio::time::timeout(grace, self.child.wait()).await {
          return waited;
        }
        tracing::warn!("native actor init {} did not stop within {grace:?} of SIGTERM; killing it", self.pid);
      }
      Err(nix::errno::Errno::ESRCH) => {}
      Err(e) => return Err(e.into()),
    }
    if let Some(group) = std::fs::read_to_string(self.layout.driver_pid()).ok().and_then(|text| text.trim().parse::<i32>().ok())
      && let Err(e) = killpg(Pid::from_raw(group), Signal::SIGKILL)
      && e != nix::errno::Errno::ESRCH
    {
      tracing::warn!("cannot kill the driving program's process group {group}: {e}");
    }
    self.child.start_kill()?;
    self.child.wait().await
  }
}
