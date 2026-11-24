//! One-use client execution bindings. Credentials use the existing task
//! transport authentication; this table prevents their use as shared resources.

use super::{
  Error, InstanceKind, InstanceState, Result, Secret, Store,
  entity::{client_execution as binding, runner},
  json, secret,
};
use needy_config::profile::ExecutorDriver;
use needy_core::types::{Capabilities, RunnerRole};
use sea_orm::{ColumnTrait as _, EntityTrait as _, IntoActiveModel as _, QueryFilter as _, Set, TransactionTrait as _};

/// Time allowed to submit a creation after reserving its holder.
// NOTICE: two minutes permits launching the same-account process and opening
// its task transport while bounding unused capability admission after CLI loss.
// This reservation deadline does not limit a bound invocation's lifetime.
pub(super) const RESERVATION_MS: i64 = 120_000;

impl Store {
  pub async fn prepare_client_execution(&self, now: i64) -> Result<(String, String, Secret)> {
    let invocation = secret::random_id();
    let name = format!("client-{}", secret::random_id());
    let credential = Secret::generate(secret::RUNNER, &name);
    let capabilities = Capabilities {
      actor: Default::default(),
      executor: [ExecutorDriver::Native].into(),
    };
    let row = runner::Model {
      name: name.clone(),
      capabilities: json(&capabilities)?,
      labels: "{}".into(),
      scope_roles: json(&[RunnerRole::Executor])?,
      scope_labels: "{}".into(),
      // Private holders are admitted directly, not by a shared runner invitation.
      join_token: String::new(),
      credential_hash: Some(credential.hash()),
      pending_hash: None,
      rotating: false,
      created_ms: now,
      last_seen_ms: now,
      removed_ms: None,
      expires_ms: None,
    };
    let txn = self.db.begin().await?;
    runner::Entity::insert(row.into_active_model()).exec(&txn).await?;
    binding::Entity::insert(binding::ActiveModel {
      holder: Set(name.clone()),
      invocation: Set(invocation.clone()),
      run: Set(None),
      created_ms: Set(now),
    })
    .exec(&txn)
    .await?;
    txn.commit().await?;
    Ok((name, invocation, credential))
  }

  pub async fn client_execution(&self, holder: &str) -> Result<Option<binding::Model>> {
    Ok(binding::Entity::find_by_id(holder).one(&self.db).await?)
  }

  pub async fn client_execution_for_run(&self, run: &str) -> Result<Option<String>> {
    Ok(binding::Entity::find().filter(binding::Column::Run.eq(run)).one(&self.db).await?.map(|b| b.holder))
  }

  pub async fn client_execution_invocation(&self, capability: &str) -> Result<String> {
    let name = self
      .authenticate(capability, needy_common::time::now_ms())
      .await?
      .ok_or_else(|| Error::Invalid("client execution capability is invalid".into()))?;
    let binding = self.client_execution(&name).await?.ok_or_else(|| Error::Invalid("not a client execution capability".into()))?;
    if binding.run.is_some() {
      return Err(Error::Invalid("client execution capability was used".into()));
    }
    Ok(binding.invocation)
  }

  /// Consumes a capability by a conditional update, so simultaneous creates
  /// cannot both claim it. An existing run never changes holders on attach.
  pub async fn bind_client_execution(&self, capability: &str, run: &str, now: i64) -> Result<String> {
    let name = self.authenticate(capability, now).await?.ok_or_else(|| Error::Invalid("client execution capability is invalid".into()))?;
    let claimed = binding::Entity::update_many()
      .col_expr(binding::Column::Run, sea_orm::sea_query::Expr::value(run))
      .filter(binding::Column::Holder.eq(&name))
      .filter(binding::Column::Invocation.eq(run))
      .filter(binding::Column::Run.is_null())
      .filter(binding::Column::CreatedMs.gt(now - RESERVATION_MS))
      .exec(&self.db)
      .await?;
    if claimed.rows_affected != 1 {
      return Err(Error::Invalid("client execution capability was used, expired, or is not a creation capability".into()));
    }
    Ok(name)
  }

  /// Used by authentication as well as heartbeat, so an unused holder expires
  /// even if the creating CLI dies before sending CreateRun.
  pub async fn client_execution_active(&self, holder: &str, now: i64) -> Result<bool> {
    let Some(binding) = self.client_execution(holder).await? else {
      return Ok(true);
    };
    match binding.run {
      None => Ok(now < binding.created_ms + RESERVATION_MS),
      Some(run) => Ok(match self.run(&run).await? {
        Some(r) => {
          if matches!(
            r.state,
            needy_core::types::RunState::Creating | needy_core::types::RunState::Running | needy_core::types::RunState::Stopping
          ) {
            true
          } else {
            // Natural completion saves the terminal run before mirror-out.
            // Its holder still serves owned cleanup until its exact executor
            // reference is released; terminal runs cannot admit new work.
            self.instances_of(&run).await?.iter().any(|instance| {
              instance.kind == InstanceKind::Executor
                && instance.state == InstanceState::Running
                && instance.runner.as_deref() == Some(holder)
            })
          }
        }
        // Claim precedes writing the run. Bound admission remains bounded if
        // the create crashes in that interval.
        None => now < binding.created_ms + RESERVATION_MS,
      }),
    }
  }
}
