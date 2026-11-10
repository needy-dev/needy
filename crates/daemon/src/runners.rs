//! The control plane's runner registry (`RunnerService`) over `state.db`:
//! join tokens, joins, heartbeats, rotation and removal.
//!
//! The control plane owns the heartbeat interval and tells runners in every
//! join and heartbeat response, so whether a runner is online is decided by
//! the same clock and interval that the runner follows.

use needy_api_client::proto::v1::{ControlRequest, ControlResponse, TaskRequest, TaskResponse};
use needy_api_server::control::{
  Beat, ControlError, CreatedJoinToken, Credential, FromRunner, JoinTokenInfo, JoinTokenState, Joined, Joining, NewJoinToken, Result,
  RunnerInfo, RunnerReport, RunnerStatus, Runners, ToRunner,
};
use needy_common::time::now_ms;

use crate::Daemon;
use crate::store::{self, Error};

/// A runner is offline after this many heartbeat intervals without one, so
/// one late or lost heartbeat (a tunnel that reconnects) does not flap it.
const MISSED_HEARTBEATS: i64 = 3;

impl Daemon {
  fn heartbeat_interval_ms(&self) -> i64 {
    i64::try_from(self.config.heartbeat_interval.as_millis()).unwrap_or(i64::MAX)
  }

  fn runner_info(&self, runner: store::Runner, now: i64) -> RunnerInfo {
    let status = match runner.removed_ms {
      Some(_) => RunnerStatus::Removed,
      None if now - runner.last_seen_ms <= MISSED_HEARTBEATS * self.heartbeat_interval_ms() => RunnerStatus::Online,
      None => RunnerStatus::Offline,
    };
    RunnerInfo {
      name: runner.name,
      roles: runner.roles,
      labels: runner.labels,
      status,
      created_ms: runner.created_ms,
      last_seen_ms: runner.last_seen_ms,
      rotating: runner.rotating,
      join_token: runner.join_token,
    }
  }
}

fn join_token_info(token: store::JoinToken, now: i64) -> JoinTokenInfo {
  let state = if token.revoked_ms.is_some() {
    JoinTokenState::Revoked
  } else if now >= token.expires_ms {
    JoinTokenState::Expired
  } else if token.uses_left == 0 {
    JoinTokenState::UsedUp
  } else {
    JoinTokenState::Valid
  };
  JoinTokenInfo {
    id: token.id,
    roles: token.scope.roles,
    labels: token.scope.labels,
    created_ms: token.created_ms,
    expires_ms: token.expires_ms,
    uses_left: token.uses_left,
    state,
  }
}

fn report(report: RunnerReport) -> store::Report {
  store::Report {
    roles: report.roles,
    labels: report.labels,
  }
}

impl Daemon {
  /// The machine `runner` last reported in a heartbeat.
  pub(crate) fn runner_host(&self, runner: &str) -> Option<needy_api_server::control::RunnerHost> {
    self.runner_hosts.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).get(runner).cloned()
  }
}

/// Each store error of the runner and client registries as one row of the
/// status table.
pub(crate) fn control_error(error: Error) -> ControlError {
  let message = error.to_string();
  match error {
    Error::InvalidJoinToken
    | Error::Expired(_)
    | Error::UsedUp(_)
    | Error::Revoked(_)
    | Error::InvalidPairingCode
    | Error::PairingCodeExpired(_)
    | Error::PairingCodeUsed(_) => ControlError::Unauthenticated(message),
    Error::NoJoinToken(_) | Error::NoRunner(_) | Error::NoClient(_) => ControlError::NotFound(message),
    Error::OutOfScope(_) => ControlError::denied(message),
    Error::RunnerExists(_) | Error::ClientExists(_) => ControlError::Failed(message),
    Error::Invalid(_) => ControlError::Invalid(message),
    Error::Schema(_) | Error::Corrupt(_) | Error::Database(_) => {
      tracing::error!("state.db: {error:#}");
      ControlError::Internal(message)
    }
  }
}

#[async_trait::async_trait]
impl Runners for Daemon {
  async fn create_join_token(&self, request: NewJoinToken) -> Result<CreatedJoinToken> {
    let now = now_ms();
    let new = store::NewJoinToken {
      scope: store::Scope {
        roles: request.roles,
        labels: request.labels,
      },
      ttl_ms: request.ttl_ms,
      uses: request.uses,
    };
    let (token, secret) = self.state.create_join_token(new, now).await.map_err(control_error)?;
    tracing::info!("join token {} created, {} uses", token.id, token.uses_left);
    Ok(CreatedJoinToken {
      info: join_token_info(token, now),
      token: secret.into_string(),
    })
  }

  async fn join_tokens(&self) -> Result<Vec<JoinTokenInfo>> {
    let now = now_ms();
    let tokens = self.state.join_tokens().await.map_err(control_error)?;
    Ok(tokens.into_iter().map(|t| join_token_info(t, now)).collect())
  }

  async fn revoke_join_token(&self, id: &str) -> Result<()> {
    self.state.revoke_join_token(id, now_ms()).await.map_err(control_error)?;
    tracing::info!("join token {id} revoked");
    Ok(())
  }

  async fn join(&self, request: Joining) -> Result<Joined> {
    let now = now_ms();
    let joined = self.state.join(&request.join_token, &request.name, report(request.report), now).await;
    let (runner, credential) = joined.map_err(|e| {
      tracing::warn!("runner {} cannot join: {e}", request.name);
      control_error(e)
    })?;
    tracing::info!("runner {} joined with join token {}", runner.name, runner.join_token);
    Ok(Joined {
      runner: self.runner_info(runner, now),
      credential: credential.into_string(),
      heartbeat_interval_ms: self.heartbeat_interval_ms(),
    })
  }

  async fn heartbeat(&self, runner: &str, mut reported: RunnerReport) -> Result<Beat> {
    let host = reported.host.take();
    let credential = self.state.heartbeat(runner, report(reported), now_ms()).await.map_err(control_error)?;
    // Only an accepted heartbeat tells the control plane about a machine.
    if let Some(host) = host {
      // NOTICE: one insert under the lock, so a panic elsewhere leaves the
      // map whole.
      self.runner_hosts.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).insert(runner.to_string(), host);
    }
    if credential.is_some() {
      tracing::info!("runner {runner}: handed out a rotated credential");
    }
    Ok(Beat {
      credential: credential.map(store::Secret::into_string),
      heartbeat_interval_ms: self.heartbeat_interval_ms(),
    })
  }

  async fn runners(&self) -> Result<Vec<RunnerInfo>> {
    let now = now_ms();
    let runners = self.state.runners().await.map_err(control_error)?;
    Ok(runners.into_iter().map(|r| self.runner_info(r, now)).collect())
  }

  async fn rotate(&self, runner: &str) -> Result<()> {
    self.state.rotate(runner).await.map_err(control_error)?;
    tracing::info!("runner {runner}: credential rotation asked for");
    Ok(())
  }

  async fn remove(&self, runner: &str) -> Result<()> {
    self.state.remove(runner, now_ms()).await.map_err(control_error)?;
    // Its control stream ends; connecting again, the runner is refused.
    self.links.close(runner);
    tracing::info!("runner {runner} removed");
    Ok(())
  }

  async fn credential(&self, token: &str) -> Credential {
    if !store::is_runner_credential(token) {
      return Credential::Other;
    }
    match self.state.authenticate(token).await {
      Ok(Some(name)) => Credential::Runner(name),
      Ok(None) => Credential::Refused,
      Err(e) => {
        tracing::error!("state.db: cannot check a runner credential: {e:#}");
        Credential::Refused
      }
    }
  }

  async fn control(&self, runner: &str, results: FromRunner<ControlRequest>) -> Result<ToRunner<ControlResponse>> {
    // The runner's report goes to `reconcile`, outside the stream's reader.
    let daemon = self.clone();
    let on_report: crate::link::OnReport = Box::new(move |runner, report| {
      let reconciling = daemon.clone();
      let runner = runner.to_string();
      daemon.tasks.spawn(async move { reconciling.reconcile_runner(&runner, report.processes).await });
    });
    Ok(self.links.open(runner, results, &self.tasks, &self.cancel, on_report))
  }

  async fn task(&self, runner: &str, task: u64, responses: FromRunner<TaskRequest>) -> Result<ToRunner<TaskResponse>> {
    self.links.task(runner, task, responses)
  }
}

#[cfg(test)]
#[path = "runners_test.rs"]
mod tests;
