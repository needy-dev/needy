//! The control plane's runner registry (`RunnerService`) over `state.db`:
//! join tokens, joins, heartbeats, rotation and removal.
//!
//! The control plane owns the heartbeat interval and tells runners in every
//! join and heartbeat response, so whether a runner is online is decided by
//! the same clock and interval that the runner follows.

use std::time::Duration;

use needy_api_client::proto::v1::{ControlRequest, ControlResponse, TaskRequest, TaskResponse};
use needy_api_server::control::{
  Beat, ControlError, CreatedJoinToken, Credential, FromRunner, JoinTokenInfo, JoinTokenState, Joined, Joining, MintRunnerCredential,
  MintedRunnerCredential, NewJoinToken, Result, RunnerInfo, RunnerReport, RunnerStatus, Runners, ToRunner,
};
use needy_common::time::now_ms;

use crate::Daemon;
use crate::static_runners::StaticRunner;
use crate::store::{self, Error};

/// A runner is offline after this many heartbeat intervals without one, so
/// one late or lost heartbeat (a tunnel that reconnects) does not flap it.
const MISSED_HEARTBEATS: i64 = 3;

/// How often the background maintenance loop checks the static runners file
/// and closes expired short-lived runner control streams.
const MAINTENANCE_TICK: Duration = Duration::from_secs(1);

impl Daemon {
  fn heartbeat_interval_ms(&self) -> i64 {
    i64::try_from(self.config.heartbeat_interval.as_millis()).unwrap_or(i64::MAX)
  }

  /// Spawns the background loop that reloads `--runners-file` and disconnects
  /// any short-lived runners whose TTL has elapsed.
  pub(crate) fn spawn_runner_maintenance(&self) {
    // Hold only the leaf handles, never `Daemon`, so dropping the last `Daemon`
    // clone cancels `maintenance_cancel` via its drop guard and releases all
    // resources promptly.
    let static_runners = self.static_runners.clone();
    let links = self.links.clone();
    let state = self.state.clone();
    let cancel = self.maintenance_cancel.clone();
    self.tasks.spawn(async move {
      let mut interval = tokio::time::interval(MAINTENANCE_TICK);
      interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
      // Skip the immediate first tick (start_with_backends already loaded).
      interval.tick().await;
      loop {
        tokio::select! {
          () = cancel.cancelled() => break,
          _ = interval.tick() => {
            static_runners.reload(&links);
            match state.expired_runners(now_ms()).await {
              Ok(expired) => {
                for name in expired {
                  if links.get(&name).is_some() {
                    tracing::info!("runner {name} credential expired; closing control stream");
                    links.close(&name);
                  }
                }
              }
              Err(e) => tracing::warn!("cannot check expired runners: {e}"),
            }
          }
        }
      }
    });
  }

  /// Schedules a targeted disconnect when a freshly minted runner credential
  /// reaches `expires_ms`, so short TTLs disconnect promptly without waiting for
  /// the periodic sweep.
  fn schedule_expiry(&self, name: String, expires_ms: i64) {
    let links = self.links.clone();
    let static_runners = self.static_runners.clone();
    let state = self.state.clone();
    let cancel = self.maintenance_cancel.clone();
    self.tasks.spawn(async move {
      let wait_ms = expires_ms.saturating_sub(now_ms());
      if wait_ms > 0 {
        tokio::select! {
          () = cancel.cancelled() => return,
          () = tokio::time::sleep(Duration::from_millis(wait_ms as u64)) => {}
        }
      }
      if cancel.is_cancelled() || static_runners.contains(&name) {
        return;
      }
      // Re-check current store state before disconnecting so a runner that was
      // revoked and re-minted with a longer TTL under the same name is not
      // killed by the stale timer.
      if matches!(state.check_active(&name, now_ms()).await, Err(Error::RunnerExpired(_))) && links.get(&name).is_some() {
        tracing::info!("runner {name} credential expired; closing control stream");
        links.close(&name);
      }
    });
  }

  /// Whether `runner` joined (or is statically configured), was not removed or
  /// expired, and sends heartbeats, as `runner ls` shows it online.
  pub(crate) async fn runner_online(&self, runner: &str) -> bool {
    self.static_runners.reload(&self.links);
    let now = now_ms();
    if let Some(entry) = self.static_runners.get(runner) {
      return self.static_runner_info(entry, now).status == RunnerStatus::Online;
    }
    let runners = match self.state.runners().await {
      Ok(runners) => runners,
      Err(e) => {
        tracing::warn!("cannot read the runner registry: {e}");
        return false;
      }
    };
    runners.into_iter().find(|r| r.name == runner).is_some_and(|r| self.runner_info(r, now).status == RunnerStatus::Online)
  }

  fn runner_info(&self, runner: store::Runner, now: i64) -> RunnerInfo {
    let expired = runner.expires_ms.is_some_and(|exp| now >= exp);
    let status = match runner.removed_ms {
      Some(_) => RunnerStatus::Removed,
      None if expired => RunnerStatus::Removed,
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
      expires_ms: runner.expires_ms,
      static_config: false,
    }
  }

  fn static_runner_info(&self, runner: StaticRunner, now: i64) -> RunnerInfo {
    let status = if runner.last_seen_ms > 0 && now - runner.last_seen_ms <= MISSED_HEARTBEATS * self.heartbeat_interval_ms() {
      RunnerStatus::Online
    } else {
      RunnerStatus::Offline
    };
    RunnerInfo {
      host: self.runner_host(&runner.name),
      name: runner.name,
      capabilities: runner.capabilities,
      labels: runner.labels,
      status,
      created_ms: runner.created_ms,
      last_seen_ms: runner.last_seen_ms,
      rotating: false,
      join_token: String::new(),
      expires_ms: None,
      static_config: true,
    }
  }
}

impl Daemon {
  /// What an unprefixed bearer token is to the runner registry: only a
  /// static runner's credential can be one, found by its hash.
  fn unprefixed_runner(&self, token: &str) -> Credential {
    self.static_runners.reload(&self.links);
    let Some(name) = self.static_runners.find(token) else {
      return Credential::Other;
    };
    self.static_clients.reload();
    if let Some(client) = self.static_clients.holds(token) {
      tracing::warn!("static runner {name} and static client {client} have the same token; refusing it for both");
      return Credential::Refused;
    }
    Credential::Runner(name)
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
    needy_common::sync::lock(&self.runner_hosts).get(runner).cloned()
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
    Error::RunnerExpired(name) => ("runner.expired", vec![("runner".into(), name.clone())]),
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
    | Error::PairingCodeUsed(_)
    | Error::RunnerExpired(_) => ControlError::Unauthenticated(message),
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
    self.static_runners.reload(&self.links);
    let now = now_ms();
    let host = reported.host.take();
    if self.static_runners.heartbeat(runner, report(reported.clone()), now).map_err(|error| control_error(error, false))? {
      if let Some(host) = host {
        needy_common::sync::lock(&self.runner_hosts).insert(runner.to_string(), host);
      }
      return Ok(Beat {
        credential: None,
        heartbeat_interval_ms: self.heartbeat_interval_ms(),
      });
    }
    if !self.state.client_execution_active(runner, now).await.map_err(|e| control_error(e, false))? {
      return Err(ControlError::Unauthenticated("client execution invocation ended or reservation expired".into()));
    }
    let credential = match self.state.heartbeat(runner, report(reported), now).await {
      Ok(credential) => credential,
      Err(error) => {
        if matches!(error, Error::RunnerExpired(_)) {
          self.links.close(runner);
        }
        return Err(control_error(error, true));
      }
    };
    // Only an accepted heartbeat tells the control plane about a machine.
    if let Some(host) = host {
      needy_common::sync::lock(&self.runner_hosts).insert(runner.to_string(), host);
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
    self.static_runners.reload(&self.links);
    let now = now_ms();
    let runners = self.state.runners().await.map_err(|error| control_error(error, false))?;
    let mut merged: std::collections::BTreeMap<String, RunnerInfo> = std::collections::BTreeMap::new();
    for runner in runners {
      if self.state.client_execution(&runner.name).await.map_err(|e| control_error(e, false))?.is_none() {
        let info = self.runner_info(runner, now);
        merged.insert(info.name.clone(), info);
      }
    }
    for runner in self.static_runners.list() {
      let info = self.static_runner_info(runner, now);
      merged.insert(info.name.clone(), info);
    }
    Ok(merged.into_values().collect())
  }

  async fn rotate(&self, runner: &str) -> Result<()> {
    self.static_runners.reload(&self.links);
    if self.static_runners.contains(runner) {
      return Err(ControlError::Failed(format!("runner {runner} is statically configured and cannot be rotated")));
    }
    self.state.rotate(runner, now_ms()).await.map_err(|error| control_error(error, true))?;
    tracing::info!("runner {runner}: credential rotation asked for");
    Ok(())
  }

  async fn remove(&self, runner: &str) -> Result<()> {
    self.static_runners.reload(&self.links);
    if self.static_runners.contains(runner) {
      return Err(ControlError::Failed(format!("runner {runner} is statically configured and cannot be removed")));
    }
    self.state.remove(runner, now_ms()).await.map_err(|error| control_error(error, true))?;
    // Its control stream ends; connecting again, the runner is refused.
    self.links.close(runner);
    tracing::info!("runner {runner} removed");
    Ok(())
  }

  async fn mint_credential(&self, request: MintRunnerCredential) -> Result<MintedRunnerCredential> {
    self.static_runners.reload(&self.links);
    if self.static_runners.contains(&request.name) {
      return Err(ControlError::Failed(format!("runner {} is statically configured and cannot be minted dynamically", request.name)));
    }
    let now = now_ms();
    let scope = store::Scope {
      roles: request.roles,
      labels: request.labels,
    };
    let (runner, credential) =
      self.state.mint_runner(&request.name, request.ttl_ms, scope, now).await.map_err(|error| control_error(error, true))?;
    let expires_ms = runner.expires_ms.unwrap_or_else(|| now.saturating_add(request.ttl_ms));
    self.schedule_expiry(runner.name.clone(), expires_ms);
    tracing::info!("minted short-lived credential for runner {}, expires at {expires_ms}", runner.name);
    Ok(MintedRunnerCredential {
      runner: self.runner_info(runner, now),
      credential: credential.into_string(),
      expires_ms,
      heartbeat_interval_ms: self.heartbeat_interval_ms(),
    })
  }

  async fn revoke_credential(&self, runner: &str) -> Result<()> {
    self.static_runners.reload(&self.links);
    if self.static_runners.contains(runner) {
      return Err(ControlError::Failed(format!("runner {runner} is statically configured and cannot be revoked")));
    }
    self.state.remove(runner, now_ms()).await.map_err(|error| control_error(error, true))?;
    self.links.close(runner);
    tracing::info!("runner {runner} credential revoked");
    Ok(())
  }

  async fn credential(&self, token: &str) -> Result<Credential> {
    if store::is_client_token(token) {
      return Ok(Credential::Other);
    }
    if !store::is_runner_credential(token) {
      return Ok(self.unprefixed_runner(token));
    }
    self.static_runners.reload(&self.links);
    if let Some((name, matched)) = self.static_runners.authenticate(token) {
      return Ok(if matched {
        Credential::Runner(name)
      } else {
        Credential::Refused
      });
    }
    let now = now_ms();
    Ok(match self.state.authenticate(token, now).await {
      Ok(Some(name)) => {
        if self.state.client_execution_active(&name, now).await.map_err(|e| control_error(e, false))? {
          Credential::Runner(name)
        } else {
          Credential::Refused
        }
      }
      Ok(None) => Credential::Refused,
      Err(e) => {
        tracing::error!("state.db: cannot check a runner credential: {e:#}");
        return Err(ControlError::Unavailable("authentication registry unavailable".into()).with_diagnostic("auth.backend_unavailable", []));
      }
    })
  }

  async fn control(&self, runner: &str, results: FromRunner<ControlRequest>) -> Result<ToRunner<ControlResponse>> {
    self.static_runners.reload(&self.links);
    let now = now_ms();
    // A runner connecting, such as again after a restart of the control
    // plane, is seen now, not at its next heartbeat.
    if !self.static_runners.seen(runner, now)
      && let Err(error) = self.state.seen(runner, now).await
    {
      if matches!(error, Error::RunnerExpired(_)) {
        self.links.close(runner);
      }
      return Err(control_error(error, true));
    }
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
    self.static_runners.reload(&self.links);
    if !self.static_runners.contains(runner)
      && let Err(error) = self.state.check_active(runner, now_ms()).await
    {
      if matches!(error, Error::RunnerExpired(_)) {
        self.links.close(runner);
      }
      return Err(control_error(error, false));
    }
    self.links.task(runner, task, responses)
  }
}

#[cfg(test)]
#[path = "runners_test.rs"]
mod tests;
