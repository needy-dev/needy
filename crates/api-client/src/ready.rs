//! Waiting until a server answers: a container's executor binding its socket,
//! or the daemon's guest listener while the actor boots.

use std::time::Duration;

use backon::{ConstantBuilder, Retryable};
use needy_api_proto::v1::CheckRequest;

use crate::{Client, Error, Result};

// NOTICE: 100 ms apart is short next to a container or VM start and long
// enough not to busy-loop on a missing socket.
const DELAY: Duration = Duration::from_millis(100);

/// Connects to `endpoint` and returns once its health check answers, retrying
/// for up to `within` while the server is not up yet. An error that a retry
/// cannot fix, such as a malformed endpoint, returns at once.
pub async fn wait(endpoint: &str, within: Duration) -> Result<Client> {
  let attempt = || async {
    let client = Client::connect(endpoint).await?;
    // Any answer, even an error status, means the server is up.
    match client.health().check(CheckRequest {}).await {
      Err(status) if status.code() == tonic::Code::Unavailable => Err(Error::Status(status)),
      _ => Ok(client),
    }
  };
  let times = (within.as_millis() / DELAY.as_millis()).max(1) as usize;
  let retry = ConstantBuilder::default().with_delay(DELAY).with_max_times(times);
  attempt.retry(retry).when(Error::is_transient).await.map_err(|error| match error {
    error if error.is_transient() => Error::Unreachable {
      endpoint: endpoint.to_string(),
      message: error.to_string(),
    },
    error => error,
  })
}

// The tests serve and dial Unix sockets.
#[cfg(all(test, unix))]
#[path = "ready_test.rs"]
mod tests;

/// Waits for a successful health response from the expected process instance
/// and role. Every attempt and the whole operation share the caller's deadline.
pub async fn wait_expected(endpoint: &str, within: Duration, role: &str, instance_id: &str) -> Result<Client> {
  let deadline = tokio::time::Instant::now() + within;
  let mut message = "no successful health response".to_string();
  loop {
    let attempt = async {
      let client = Client::connect(endpoint).await?;
      let health = client.health().check(CheckRequest {}).await?.into_inner();
      if health.role != role || health.instance_id != instance_id {
        return Err(Error::IdentityMismatch {
          expected_role: role.into(),
          expected_instance: instance_id.into(),
          role: health.role,
          instance: health.instance_id,
        });
      }
      Ok(client)
    };
    match tokio::time::timeout_at(deadline, attempt).await {
      Ok(Ok(client)) => return Ok(client),
      Ok(Err(error @ (Error::InvalidEndpoint(_) | Error::IdentityMismatch { .. }))) => return Err(error),
      Ok(Err(error)) => message = error.to_string(),
      Err(_) => break,
    }
    if tokio::time::Instant::now() + DELAY >= deadline {
      break;
    }
    tokio::time::sleep(DELAY).await;
  }
  Err(Error::Unreachable {
    endpoint: endpoint.into(),
    message,
  })
}
