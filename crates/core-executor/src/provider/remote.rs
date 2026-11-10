//! The `remote` runtime: another machine's needy daemon, managed by its
//! owner. Creating an instance connects to it; destroying one disconnects.
//
// TODO(runner-select): replaced by runner executors. Runners dial the
// control plane (slice 4 of the runners-and-runs design), but profiles
// select runners only with slice 5; until then a profile reaches another
// machine through this provider and a `needy daemon --mode executor` there.

use needy_api_proto::v1::CheckRequest;

use super::{Error, InstanceHandle, InstanceSpec, Provider, connect, sftp_command};

#[derive(Default)]
pub struct Remote;

#[async_trait::async_trait]
impl Provider for Remote {
  async fn create(&self, spec: &InstanceSpec) -> Result<InstanceHandle, Error> {
    let remote = spec.config.remote.as_ref().ok_or_else(|| Error::Config("a remote executor needs `remote`".into()))?;
    let mut client = connect(&remote.url).await?;
    if let Some(name) = &remote.token_env {
      let token = std::env::var(name).map_err(|_| Error::MissingEnv { name: name.clone() })?;
      client = client.with_token(&token)?;
    }
    // The platform picks the SFTP server; an executor that does not say
    // gets the Linux one.
    let platform = match client.health().check(CheckRequest {}).await {
      Ok(response) => response.into_inner().platform,
      Err(status) => {
        tracing::warn!("executor {}: health check: {}", spec.name, status.message());
        String::new()
      }
    };
    Ok(InstanceHandle {
      runtime: "remote",
      id: remote.url.clone(),
      executor: std::sync::Arc::new(client),
      sftp: sftp_command(&platform),
      workspace: spec.config.workspace.clone().unwrap_or_default(),
    })
  }

  /// The remote machine outlives its runs. The control daemon ends a
  /// run's processes there before it releases the instance.
  async fn destroy(&self, _handle: &InstanceHandle) -> Result<(), Error> {
    Ok(())
  }
}
