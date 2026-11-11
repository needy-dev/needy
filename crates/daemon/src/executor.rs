//! The executor role: this machine's processes, served to the CLI or a
//! control daemon until a termination signal.

use anyhow::{Context, Result, bail};
use needy_api_server::control::Caller;
use needy_api_server::{Health, Listen, Server};
use needy_core_executor::process::Host;
use needy_core_executor::service::ExecutorService;

/// Serves `ProcessService` as the executor `name` on `listen` (`unix:/path`
/// or `HOST:PORT`); clients present `token` when one is set. On a
/// termination signal the listener stops, then the processes are killed. A
/// second termination signal ends that where it is and fails, naming the
/// processes still running; the caller should then exit.
pub async fn serve(listen: &str, name: String, token: Option<String>) -> Result<()> {
  let termination = needy_api_server::termination::Termination::listen().context("cannot handle termination signals")?;
  let health = Health {
    instance_id: uuid::Uuid::new_v4().to_string(),
    role: "executor",
  };
  let host = Host::default();
  let service = ExecutorService {
    host: host.clone(),
    name,
  };
  let server = Server::new(health, Caller::Operator).token(token).process_service(service).bind(&[Listen::parse(listen)]).await?;
  // The bound address, not `listen`: a port 0 is the one the system picked.
  for address in server.addresses() {
    tracing::info!("executor listening on {address}");
  }
  let ended = termination
    .unless_forced(async {
      let served = server.serve(termination.stop()).await;
      host.shutdown().await;
      served
    })
    .await;
  match ended {
    Some(served) => Ok(served?),
    None => {
      let left: Vec<String> = host.list().iter().filter(|p| p.exit().is_none()).map(|p| p.id.clone()).collect();
      let message = if left.is_empty() {
        "exited at a second termination signal before the shutdown ended".to_string()
      } else {
        format!("exited at a second termination signal; these processes were not killed: {}", left.join(", "))
      };
      tracing::error!("{message}");
      bail!(message)
    }
  }
}
