//! Automatic client execution holder. A detached child inherits this process's
//! OS account; control credentials never confer authority to become another user.

use crate::{cli::Context, connection::Resolved};
use anyhow::{Context as _, Result};
use needy_api_client::Client;
#[cfg(unix)]
use needy_api_client::proto::v1::PrepareClientExecutionRequest;
#[cfg(unix)]
use needy_api_server::control::RunnerHost;
#[cfg(unix)]
use needy_api_server::protocol::domain::proto_runner_host;
use needy_daemon::runner::ClientExecutionConfig;
use std::path::{Path, PathBuf};
use std::process::Child;
#[cfg(unix)]
use std::process::{Command, Stdio};

pub(super) struct Holder {
  pub capability: String,
  child: Option<Child>,
  directory: PathBuf,
  kept: bool,
}

impl Holder {
  pub fn keep(&mut self) {
    // std::process::Child has no kill-on-drop. The holder now belongs to the
    // invocation; its protected startup directory is reclaimed by the child.
    self.child.take();
    self.kept = true;
  }
}

impl Drop for Holder {
  fn drop(&mut self) {
    if self.kept {
      return;
    }
    if let Some(child) = &mut self.child {
      if let Err(error) = child.kill() {
        tracing::debug!("cannot stop failed creation's execution holder: {error}");
      }
      if let Err(error) = child.wait() {
        tracing::warn!("cannot reap failed creation's execution holder: {error}");
      }
    }
    if let Err(error) = std::fs::remove_dir_all(&self.directory)
      && error.kind() != std::io::ErrorKind::NotFound
    {
      tracing::warn!("cannot remove failed holder startup directory: {error}");
    }
  }
}

#[cfg(unix)]
pub(super) async fn launch(ctx: &Context, selection: &Resolved, client: &Client) -> Result<Holder> {
  let reserved = client
    .runs()
    .prepare_client_execution(PrepareClientExecutionRequest {
      host: Some(proto_runner_host(&RunnerHost {
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        home: needy_core_executor::driver::home(),
        cpu: Default::default(),
      })),
    })
    .await
    .map_err(needy_api_client::Error::from)?
    .into_inner();
  let directory = ctx.paths()?.runtime.join("client-executions").join(uuid::Uuid::new_v4().simple().to_string());
  #[cfg(unix)]
  needy_api_server::create_private_dir(&directory)?;
  #[cfg(not(unix))]
  std::fs::create_dir_all(&directory)?;
  let directory = directory.canonicalize()?;
  let mut holder = Holder {
    capability: reserved.capability.clone(),
    child: None,
    directory: directory.clone(),
    kept: false,
  };
  let config = ClientExecutionConfig {
    control: selection.address.clone(),
    command: selection.command.clone(),
    holder: reserved.holder,
    capability: reserved.capability.clone(),
    run: reserved.run_id,
    directory: directory.clone(),
  };
  let startup = directory.join("startup.json");
  let mut options = std::fs::OpenOptions::new();
  options.write(true).create_new(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.mode(0o600);
  }
  use std::io::Write as _;
  options.open(&startup)?.write_all(&serde_json::to_vec(&config)?)?;
  let mut command = Command::new(std::env::current_exe().context("cannot locate needy for client execution")?);
  command.args(["run", "execution-holder", "--startup-file"]).arg(&startup).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
  #[cfg(unix)]
  {
    use std::os::unix::process::CommandExt as _;
    // SAFETY: setsid is async-signal-safe and touches no Rust state after fork.
    unsafe {
      command.pre_exec(|| {
        if libc::setsid() == -1 {
          return Err(std::io::Error::last_os_error());
        }
        Ok(())
      });
    }
  }
  holder.child = Some(command.spawn().context("cannot launch this account's client execution holder")?);
  Ok(holder)
}

pub(super) async fn serve(path: &Path) -> Result<i32> {
  let config: ClientExecutionConfig =
    serde_json::from_str(&crate::configuration::read_secret(path).context("cannot read private holder startup")?)?;
  let parent = path.parent().context("holder startup has no directory")?.canonicalize()?;
  anyhow::ensure!(parent == config.directory, "holder startup directory does not match its protected parent");
  // Consume the startup secret before accepting work. The capability thereafter
  // lives only in the holder process and in the daemon as a hash.
  std::fs::remove_file(path).context("cannot remove consumed holder startup")?;
  let directory = config.directory.clone();
  let served = needy_daemon::runner::serve_client_execution(config).await;
  if let Err(error) = std::fs::remove_dir_all(&directory)
    && error.kind() != std::io::ErrorKind::NotFound
  {
    tracing::warn!("cannot remove client execution holder directory: {error}");
  }
  served?;
  Ok(0)
}

// TODO(windows-client-execution): protected credential/config ACL handling is
// outside this slice; enable automatic holders after canonical client admission
// can validate and create same-account private startup files on Windows.
#[cfg(not(unix))]
pub(super) async fn launch(_ctx: &Context, _selection: &Resolved, _client: &Client) -> Result<Holder> {
  anyhow::bail!("automatic client execution requires private startup-file support on this platform")
}
