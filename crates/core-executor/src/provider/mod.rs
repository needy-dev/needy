//! Executor providers: the runtimes that create and destroy executor
//! instances. An instance is a machine that serves `ProcessService`; the
//! daemon's pool decides when to create one and when to destroy it.
//!
//! - [`native`]: this machine, through the daemon's in-process executor.
//! - [`remote`]: another machine's needy daemon.
//! - [`docker`]: a container running `needy daemon --mode executor`.
//!
//! TODO(provider): `containerd`, `firecracker` and `k8s` runtimes are new
//! files here; each needs owner approval of its slice. Until then the daemon
//! refuses those runtimes.

pub mod docker;
pub mod native;
pub mod remote;

use std::path::{Path, PathBuf};
use std::time::Duration;

use std::sync::Arc;

use needy_api_client::Client;
use needy_config::profile::{Executor, User};

pub use docker::Docker;
pub use native::Native;
pub use remote::Remote;

#[derive(Debug, thiserror::Error)]
pub enum Error {
  /// The executor's profile entry cannot be run as written.
  #[error("{0}")]
  Config(String),
  #[error("{name} is not set")]
  MissingEnv { name: String },
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

/// What the pool asks a provider for: one instance of a profile's executor.
#[derive(Debug, Clone)]
pub struct InstanceSpec {
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
pub struct InstanceHandle {
  /// `native`, `remote`, `docker`, or `runner` for a runner's own machine.
  pub runtime: &'static str,
  /// The machine (`local`), the remote URL, the container name or the
  /// runner's name.
  pub id: String,
  pub executor: Arc<dyn needy_api_client::Executor>,
  /// The SFTP server command a workspace mount runs on the instance.
  pub sftp: Vec<String>,
  /// The working directory of a command that names none; empty for the
  /// executor's own default.
  pub workspace: String,
}

#[async_trait::async_trait]
pub trait Provider: Send + Sync {
  /// Creates an instance and returns once it answers.
  async fn create(&self, spec: &InstanceSpec) -> Result<InstanceHandle, Error>;

  /// Destroys an instance this provider created. Instances that the
  /// provider does not own (this machine, a remote daemon) are left alone.
  async fn destroy(&self, handle: &InstanceHandle) -> Result<(), Error>;
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

/// The SFTP server for a platform as `CheckResponse.platform` names it.
fn sftp_command(platform: &str) -> Vec<String> {
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
