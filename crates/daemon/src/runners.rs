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

  /// Whether `runner` joined, was not removed and sends heartbeats, as
  /// `runner ls` shows it online.
  pub(crate) async fn runner_online(&self, runner: &str) -> bool {
    let runners = match self.state.runners().await {
      Ok(runners) => runners,
      Err(e) => {
        tracing::warn!("cannot read the runner registry: {e}");
        return false;
      }
    };
    let now = now_ms();
    runners.into_iter().find(|r| r.name == runner).is_some_and(|r| self.runner_info(r, now).status == RunnerStatus::Online)
  }

  fn runner_info(&self, runner: store::Runner, now: i64) -> RunnerInfo {
    let status = match runner.removed_ms {
      Some(_) => RunnerStatus::Removed,
      None if now - runner.last_seen_ms <= MISSED_HEARTBEATS * self.heartbeat_interval_ms() => RunnerStatus::Online,
      None => RunnerStatus::Offline,
    };
    RunnerInfo {
      host: self.runner_host(&runner.name),
      name: runner.name,
      capabilities: runner.capabilities,
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
    capabilities: report.capabilities,
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
pub(crate) fn control_error(error: Error, mutation: bool) -> ControlError {
  let message = error.to_string();
  let (reason, facts) = match &error {
    Error::InvalidJoinToken | Error::InvalidPairingCode => ("bootstrap.invalid_code", vec![]),
    Error::Expired(id) | Error::PairingCodeExpired(id) => ("bootstrap.expired", vec![("id".into(), id.clone())]),
    Error::UsedUp(id) | Error::PairingCodeUsed(id) => ("bootstrap.used", vec![("id".into(), id.clone())]),
    Error::Revoked(id) => ("bootstrap.revoked", vec![("id".into(), id.clone())]),
    Error::OutOfScope(_) => ("bootstrap.scope_denied", vec![]),
    Error::NoJoinToken(_) => ("bootstrap.invalid_code", vec![]),
    Error::NoRunner(name) => ("runner.not_found", vec![("runner".into(), name.clone())]),
    Error::NoClient(id) => ("client.not_found", vec![("client".into(), id.clone())]),
    Error::RunnerExists(name) => ("runner.name_in_use", vec![("runner".into(), name.clone())]),
    Error::PairingCodeOpen(name) => ("client.invitation_open", vec![("client".into(), name.clone())]),
    Error::Schema(_) | Error::Corrupt(_) => ("state.read_failed", vec![("store".into(), "state.db".into())]),
    Error::Database(_) => (
      if mutation {
        "state.write_failed"
      } else {
        "state.read_failed"
      },
      vec![("store".into(), "state.db".into())],
    ),
    Error::Invalid(_) | Error::Refused(_) => ("", vec![]),
  };
  let result = match error {
    Error::InvalidJoinToken
    | Error::Expired(_)
    | Error::UsedUp(_)
    | Error::Revoked(_)
    | Error::InvalidPairingCode
    | Error::PairingCodeExpired(_)
    | Error::PairingCodeUsed(_) => ControlError::Unauthenticated(message),
    Error::NoJoinToken(_) | Error::NoRunner(_) | Error::NoClient(_) => ControlError::NotFound(message),
    Error::OutOfScope(_) => ControlError::denied(message),
    Error::RunnerExists(_) | Error::PairingCodeOpen(_) | Error::Refused(_) => ControlError::Failed(message),
    Error::Invalid(_) => ControlError::Invalid(message),
    Error::Schema(_) | Error::Corrupt(_) | Error::Database(_) => {
      tracing::error!("state.db: {error:#}");
      ControlError::Internal(message)
    }
  };
  if reason.is_empty() {
    result
  } else {
    result.with_diagnostic(reason, facts)
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
    let (token, secret) = self.state.create_join_token(new, now).await.map_err(|error| control_error(error, true))?;
    tracing::info!("join token {} created, {} uses", token.id, token.uses_left);
    Ok(CreatedJoinToken {
      info: join_token_info(token, now),
      token: secret.into_string(),
    })
  }

  async fn join_tokens(&self) -> Result<Vec<JoinTokenInfo>> {
    let now = now_ms();
    let tokens = self.state.join_tokens().await.map_err(|error| control_error(error, false))?;
    Ok(tokens.into_iter().map(|t| join_token_info(t, now)).collect())
  }

  async fn revoke_join_token(&self, id: &str) -> Result<()> {
    self.state.revoke_join_token(id, now_ms()).await.map_err(|error| control_error(error, true))?;
    tracing::info!("join token {id} revoked");
    Ok(())
  }

  async fn join(&self, request: Joining) -> Result<Joined> {
    let now = now_ms();
    let joined = self.state.join(&request.join_token, &request.name, report(request.report), now).await;
    let (runner, credential) = joined.map_err(|e| {
      tracing::warn!("runner {} cannot join: {e}", request.name);
      control_error(e, true)
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
    let credential = self.state.heartbeat(runner, report(reported), now_ms()).await.map_err(|error| control_error(error, true))?;
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
    let runners = self.state.runners().await.map_err(|error| control_error(error, false))?;
    Ok(runners.into_iter().map(|r| self.runner_info(r, now)).collect())
  }

  async fn rotate(&self, runner: &str) -> Result<()> {
    self.state.rotate(runner).await.map_err(|error| control_error(error, true))?;
    tracing::info!("runner {runner}: credential rotation asked for");
    Ok(())
  }

  async fn remove(&self, runner: &str) -> Result<()> {
    self.state.remove(runner, now_ms()).await.map_err(|error| control_error(error, true))?;
    // Its control stream ends; connecting again, the runner is refused.
    self.links.close(runner);
    tracing::info!("runner {runner} removed");
    Ok(())
  }

  async fn credential(&self, token: &str) -> Result<Credential> {
    if !store::is_runner_credential(token) {
      return Ok(Credential::Other);
    }
    Ok(match self.state.authenticate(token).await {
      Ok(Some(name)) => Credential::Runner(name),
      Ok(None) => Credential::Refused,
      Err(e) => {
        tracing::error!("state.db: cannot check a runner credential: {e:#}");
        return Err(ControlError::Unavailable("authentication registry unavailable".into()).with_diagnostic("auth.backend_unavailable", []));
      }
    })
  }

  async fn control(&self, runner: &str, results: FromRunner<ControlRequest>) -> Result<ToRunner<ControlResponse>> {
    // A runner connecting, such as again after a restart of the control
    // plane, is seen now, not at its next heartbeat.
    self.state.seen(runner, now_ms()).await.map_err(|error| control_error(error, true))?;
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
