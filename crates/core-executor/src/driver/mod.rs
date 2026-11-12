//! Executor drivers: what creates and destroys executor instances. A
//! profile's `executors.NAME.driver` names one. An instance is a machine that serves `ProcessService`; the
//! daemon's pool decides when to create one and when to destroy it.
//!
//! - [`native`]: this machine, through the daemon's in-process executor.
//! - [`docker`] (`linux-container`): a container running `needy daemon
//!   --mode executor`, through a Docker-compatible engine.
//!
//! A runner's own machine needs no driver: the daemon reaches it over the
//! runner's streams. The `remote` provider, which dialed another machine's
//! `needy daemon --mode executor`, was removed when profiles began to select
//! runners (slice 5 of the runners-and-runs design).
//!
//! TODO(executor-driver): later drivers (`linux-firecracker`, `linux-native`,
//! a Kubernetes one) are new files here; each needs owner approval of its
//! slice, see
//! `docs/ai/references/architecture/2026-11-12-drivers-and-extension-points-design.md`.

pub mod docker;
pub mod native;

use std::path::{Path, PathBuf};
use std::time::Duration;

use std::sync::Arc;

use needy_api_client::Client;
use needy_config::profile::{Executor, User};

pub use docker::LinuxContainer;
pub use native::Native;

#[derive(Debug, thiserror::Error)]
pub enum Error {
  /// The executor's profile entry cannot be run as written.
  #[error("{0}")]
  Config(String),
  #[error("cannot create {path}")]
  Directory {
    path: PathBuf,
    source: std::io::Error,
  },
  /// A directory shared with a container cannot be given to the
  /// container's user.
  #[error(
    "cannot give {path} to the container's user {uid}:{gid}, so its executor cannot create its socket there; run the daemon as root or as that user"
  )]
  Ownership {
    path: PathBuf,
    uid: u32,
    gid: u32,
    source: std::io::Error,
  },
  #[error(transparent)]
  Client(#[from] needy_api_client::Error),
  #[error("docker")]
  Docker(#[from] bollard::errors::Error),
}

/// What the pool asks a driver for: one instance of a profile's executor.
#[derive(Debug, Clone)]
pub struct Spec {
  /// The executor's name in the profile.
  pub name: String,
  /// The pool key the instance serves; names its container and directories.
  pub key: String,
  pub run: String,
  pub config: Executor,
  /// The profile's directory; relative paths in `config` resolve against it.
  pub base: PathBuf,
  /// The actor's user, who owns the workspace on both sides.
  pub user: User,
  /// The actor's hostname, so `uname -n` agrees on both sides.
  pub hostname: Option<String>,
}

/// A created instance: how to reach it and what the daemon needs to know
/// about it.
#[derive(Clone)]
pub struct Instance {
  /// The driver that created it (`native`, `linux-container`), or `runner`
  /// for a runner's own machine, which no driver creates.
  pub driver: &'static str,
  /// The machine (`local`), the container name or the runner's name.
  pub id: String,
  pub executor: Arc<dyn needy_api_client::Executor>,
  /// The SFTP server command a workspace mount runs on the instance.
  pub sftp: Vec<String>,
  /// The working directory of a command that names none; empty for the
  /// executor's own default.
  pub workspace: String,
}

#[async_trait::async_trait]
pub trait Driver: Send + Sync {
  /// Creates an instance and returns once it answers.
  async fn create(&self, spec: &Spec) -> Result<Instance, Error>;

  /// Destroys an instance this driver created. Instances that the driver
  /// does not own (this machine) are left alone.
  async fn destroy(&self, handle: &Instance) -> Result<(), Error>;
}

/// Connects to an executor and waits until it answers its health check.
pub async fn connect(endpoint: &str) -> Result<Client, Error> {
  // NOTICE: a container's executor needs a moment to bind its socket; 10 s
  // covers a cold container start on the Linux host.
  Ok(needy_api_client::ready::wait(endpoint, Duration::from_secs(10)).await?)
}

/// A profile path on this machine: `~` is the user's home directory, and a
/// relative path resolves against the profile's directory.
fn host_path(path: &str, base: &Path) -> PathBuf {
  let home = std::env::var("HOME").unwrap_or_default();
  let path = path.replacen('~', &home, 1);
  if Path::new(&path).is_absolute() {
    PathBuf::from(path)
  } else {
    base.join(path)
  }
}

/// The SFTP server for a platform as `CheckResponse.platform` names it, or
/// as `std::env::consts::OS` does (`macos`, `windows`, `linux`).
pub fn sftp_command(platform: &str) -> Vec<String> {
  if platform.starts_with("macos") {
    vec!["/usr/libexec/sftp-server".into()]
  } else if platform.starts_with("windows") {
    vec![r"C:\Windows\System32\OpenSSH\sftp-server.exe".into()]
  } else {
    vec!["/usr/lib/openssh/sftp-server".into()]
  }
}

/// A key as a container name or directory name: anything but ASCII
/// letters, digits and `-` becomes `-`.
fn sanitize(key: &str) -> String {
  key
    .chars()
    .map(|c| {
      if c.is_ascii_alphanumeric() || c == '-' {
        c
      } else {
        '-'
      }
    })
    .collect()
}
