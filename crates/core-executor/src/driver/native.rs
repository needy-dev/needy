//! The `native` driver: this machine, through the daemon's own executor.
//! There is nothing to create; every instance shares the machine.

use needy_api_client::Client;
use needy_config::profile::ExecutorDriver;

use super::{Driver, Error, Instance, Spec, host_path, sftp_command};

pub struct Native {
  /// The in-process executor of this machine.
  client: Client,
}

impl Native {
  pub fn new(client: Client) -> Self {
    Self { client }
  }

  /// This machine with the executor's default working directory.
  pub fn machine(&self) -> Instance {
    Instance {
      driver: "native",
      id: "local".into(),
      executor: std::sync::Arc::new(self.client.clone()),
      sftp: sftp_command(&format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH)),
      workspace: String::new(),
    }
  }
}

#[async_trait::async_trait]
impl Driver for Native {
  fn kind(&self) -> ExecutorDriver {
    ExecutorDriver::Native
  }

  // NOTICE: a native executor's commands run as the user the runner (or the
  // control daemon) runs as, with that user's account files: the profile's
  // `user` applies to actors and container executors only, since needy does
  // not own this machine's accounts.
  async fn create(&self, spec: &Spec) -> Result<Instance, Error> {
    let mut handle = self.machine();
    if let Some(workspace) = &spec.config.workspace {
      handle.workspace = host_path(workspace, &spec.base).display().to_string();
    }
    Ok(handle)
  }

  /// This machine outlives its runs.
  async fn destroy(&self, _handle: &Instance) -> Result<(), Error> {
    Ok(())
  }

  /// Ends nothing: the processes a daemon's own executor ran die with a
  /// daemon that stops.
  // TODO(runner-ownership): a daemon killed with SIGKILL leaves the processes
  // its own executor ran orphaned and unknown to its successor, which does
  // not record their host PIDs. Native executors on the control host go
  // through its local runner (`--local-runner`) to be owned like any
  // runner's.
  async fn reconcile(&self, _id: &str) -> bool {
    false
  }
}
