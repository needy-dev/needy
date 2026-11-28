//! Automatic client execution holder. A detached child inherits this process's
//! OS account; control credentials never confer authority to become another user.

use crate::{cli::Context, connection::Resolved};
use anyhow::{Context as _, Result};
use needy_api_client::Client;
use needy_api_client::proto::v1::PrepareClientExecutionRequest;
use needy_api_server::control::RunnerHost;
use needy_api_server::protocol::domain::proto_runner_host;
use needy_daemon::runner::ClientExecutionConfig;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

pub(super) struct Holder {
  pub capability: String,
  child: Option<Child>,
  directory: PathBuf,
  kept: bool,
}

impl Holder {
  pub fn keep(&mut self) {
    // std::process::Child has no kill-on-drop. The holder now belongs to the
    // invocation; its runtime directory is reclaimed by the child.
    // Transfer wait ownership independently of the CLI's attach lifetime.
    // Keeping work alive must not leave an exited child without a waiter.
    if let Some(mut child) = self.child.take() {
      std::thread::spawn(move || {
        if let Err(error) = child.wait() {
          tracing::warn!("cannot reap the client execution holder: {error}");
        }
      });
    }
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
      tracing::warn!("cannot remove failed holder runtime directory: {error}");
    }
  }
}

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
    headers: selection.headers().clone(),
    holder: reserved.holder,
    capability: reserved.capability.clone(),
    run: reserved.run_id,
    directory: directory.clone(),
  };
  let payload = serde_json::to_vec(&config)?;
  let mut command = Command::new(std::env::current_exe().context("cannot locate needy for client execution")?);
  command
    .args(["run", "execution-holder", "--startup-stdin", "--directory"])
    .arg(&directory)
    .stdin(Stdio::piped())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
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
  #[cfg(windows)]
  {
    use std::os::windows::process::CommandExt as _;
    // Reference: https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags
    // DETACHED_PROCESS (0x0000_0008) | CREATE_NEW_PROCESS_GROUP (0x0000_0200)
    // Detach the holder from the creating console so Ctrl-C / detach / terminal
    // close does not terminate the bound client execution helper.
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
  }
  let mut child = command.spawn().context("cannot launch this account's client execution holder")?;
  // Deliver the startup capability and headers exclusively over the anonymous
  // stdin pipe so no secret touches the filesystem or process environment.
  let mut stdin = child.stdin.take().context("client execution holder stdin pipe missing")?;
  let write_result = stdin.write_all(&payload);
  drop(stdin);
  holder.child = Some(child);
  write_result.context("cannot send startup configuration to client execution holder")?;
  Ok(holder)
}

pub(super) async fn serve(startup_stdin: bool, directory: &Path) -> Result<i32> {
  anyhow::ensure!(startup_stdin, "execution-holder requires --startup-stdin");
  let mut bytes = Vec::new();
  std::io::stdin().read_to_end(&mut bytes).context("cannot read holder startup from stdin")?;
  let config: ClientExecutionConfig = serde_json::from_slice(&bytes).context("cannot parse holder startup")?;
  let expected = directory.canonicalize().context("cannot resolve holder runtime directory")?;
  anyhow::ensure!(expected == config.directory, "holder runtime directory does not match startup configuration");
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
