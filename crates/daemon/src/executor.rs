//! The executor role: this machine's processes, served to the CLI or a
//! control daemon until a termination signal.

use anyhow::{Context, Result};
use needy_api_server::control::Caller;
use needy_api_server::{Health, Listen, Server};
use needy_core_executor::process::Host;
use needy_core_executor::service::ExecutorService;

/// Serves `ProcessService` as the executor `name` on `listen` (`unix:/path`
/// or `HOST:PORT`); clients present `token` when one is set. On a
/// termination signal the listener stops, then the processes are killed.
pub async fn serve(listen: &str, name: String, token: Option<String>) -> Result<()> {
  let cancel = needy_api_server::termination::token().context("cannot handle termination signals")?;
  let health = Health {
    instance_id: uuid::Uuid::new_v4().to_string(),
    role: "executor",
  };
  let host = Host::default();
  let service = ExecutorService {
    host: host.clone(),
    name,
  };
  let server = Server::new(health, Caller::Cli).token(token).process_service(service).bind(&[Listen::parse(listen)]).await?;
  tracing::info!("executor listening on {listen}");
  let served = server.serve(cancel).await;
  host.shutdown().await;
  Ok(served?)
}
