//! `state.db`: the control plane's own state, separate from `trace.db`.
//! It holds the runner registry (join tokens and the runners that joined
//! with them) and runs with the instances and processes they hold
//! ([`runs`]).
//!
//! Secrets are stored as hashes only ([`secret`]). Every time is passed in
//! by the caller in Unix milliseconds, so expiry is decided by one clock.
//!
//! Schema changes go through [`migration`]; do not issue DDL anywhere else.

mod entity;
mod migration;
mod runs;
mod secret;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use needy_core::types::RunnerRole;
use sea_orm::{
  ActiveModelTrait as _, ActiveValue::Set, ColumnTrait as _, ConnectOptions, ConnectionTrait as _, Database, DatabaseConnection, DbErr,
  EntityTrait as _, IntoActiveModel as _, QueryFilter as _, QueryOrder as _, TransactionTrait as _,
};
use sea_orm_migration::{MigratorTrait as _, SchemaManager};
use serde::Serialize;
use serde::de::DeserializeOwned;

use self::entity::{join_token, runner};
pub use self::runs::{
  InstanceKind, InstanceRecord, InstanceState, ProcessRecord, ProcessState, RunRecord, actor_instance, executor_instance,
};
pub use self::secret::Secret;

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("{0} was written by another version of needy; move it away to start afresh")]
  Schema(PathBuf),
  /// Malformed, unknown, or with a wrong secret: which of these is not
  /// said, so a guess learns nothing.
  #[error("the join token is not valid")]
  InvalidJoinToken,
  #[error("join token {0} expired")]
  Expired(String),
  #[error("join token {0} has no uses left")]
  UsedUp(String),
  #[error("join token {0} was revoked")]
  Revoked(String),
  #[error("no join token {0}")]
  NoJoinToken(String),
  /// A runner declared a role or label its join token does not allow.
  #[error("{0}")]
  OutOfScope(String),
  #[error("runner {0} exists; remove it first or join with another name")]
  RunnerExists(String),
  #[error("no runner {0}")]
  NoRunner(String),
  /// A malformed name, label or limit.
  #[error("{0}")]
  Invalid(String),
  #[error("a row of state.db cannot be read")]
  Corrupt(#[from] serde_json::Error),
  #[error(transparent)]
  Database(#[from] DbErr),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// What a join token allows: the roles a runner may declare (any when
/// empty) and the labels it gets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scope {
  pub roles: BTreeSet<RunnerRole>,
  pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewJoinToken {
  pub scope: Scope,
  pub ttl_ms: i64,
  /// How many runners may join with it.
  pub uses: u32,
}

/// A join token without its secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinToken {
  pub id: String,
  pub scope: Scope,
  pub created_ms: i64,
  pub expires_ms: i64,
  pub uses_left: u32,
  pub revoked_ms: Option<i64>,
}

/// What a runner declares when it joins and at each heartbeat.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
  pub roles: BTreeSet<RunnerRole>,
  pub labels: BTreeMap<String, String>,
}

/// A runner without its credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Runner {
  pub name: String,
  pub roles: BTreeSet<RunnerRole>,
  /// Its own labels and those its join token set.
  pub labels: BTreeMap<String, String>,
  /// What its join token allowed; heartbeats stay within it.
  pub scope: Scope,
  /// The ID of the join token it joined with.
  pub join_token: String,
  pub created_ms: i64,
  /// Its last heartbeat, or its join.
  pub last_seen_ms: i64,
  pub removed_ms: Option<i64>,
  /// A new credential was asked for and the runner has not used it yet.
  pub rotating: bool,
}

/// An open `state.db`. Clones share the connection.
#[derive(Clone)]
pub struct Store {
  db: DatabaseConnection,
}

impl Store {
  /// Opens, creating if needed, the database at `path` and applies pending
  /// migrations. A database another schema wrote is refused, not migrated.
  pub async fn open(path: &Path) -> Result<Self> {
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let mut options = ConnectOptions::new(url);
    // NOTICE: one connection serializes every transaction, so a join token's
    // check and the use it spends cannot interleave with another join. The
    // registry sees a few calls a minute per runner; it needs no
    // concurrency.
    options.sqlx_logging(false).max_connections(1).min_connections(1);
    let db = Database::connect(options).await?;
    db.execute_unprepared("PRAGMA journal_mode=WAL").await?;
    if has_other_schema(&db).await? {
      return Err(Error::Schema(path.to_path_buf()));
    }
    migration::Migrator::up(&db, None).await?;
    Ok(Self { db })
  }

  /// A join token, and its secret, which is not stored and cannot be
  /// shown again.
  pub async fn create_join_token(&self, new: NewJoinToken, now: i64) -> Result<(JoinToken, Secret)> {
    if new.ttl_ms <= 0 {
      return Err(Error::Invalid("a join token needs a positive time to live".into()));
    }
    if new.uses == 0 {
      return Err(Error::Invalid("a join token needs at least one use".into()));
    }
    check_labels(&new.scope.labels)?;
    let id = secret::random_id();
    let token = Secret::generate(secret::JOIN, &id);
    let row = join_token::Model {
      id: id.clone(),
      hash: token.hash(),
      roles: json(&new.scope.roles)?,
      labels: json(&new.scope.labels)?,
      created_ms: now,
      expires_ms: now.saturating_add(new.ttl_ms),
      uses_left: i64::from(new.uses),
      revoked_ms: None,
    };
    join_token::Entity::insert(row.clone().into_active_model()).exec(&self.db).await?;
    Ok((join_token_of(row)?, token))
  }

  /// Every join token, including expired, spent and revoked ones, oldest
  /// first.
  pub async fn join_tokens(&self) -> Result<Vec<JoinToken>> {
    let rows =
      join_token::Entity::find().order_by_asc(join_token::Column::CreatedMs).order_by_asc(join_token::Column::Id).all(&self.db).await?;
    rows.into_iter().map(join_token_of).collect()
  }

  /// Revokes a join token. Runners that joined with it keep their
  /// credentials. Revoking it again changes nothing.
  pub async fn revoke_join_token(&self, id: &str, now: i64) -> Result<()> {
    let row = join_token::Entity::find_by_id(id).one(&self.db).await?.ok_or_else(|| Error::NoJoinToken(id.to_string()))?;
    if row.revoked_ms.is_none() {
      let mut row = row.into_active_model();
      row.revoked_ms = Set(Some(now));
      row.update(&self.db).await?;
    }
    Ok(())
  }

  /// Registers the runner `name` with a join token and returns its
  /// credential, which is not stored and cannot be shown again. A join
  /// that fails spends no use of the token. A removed runner's name may
  /// join again.
  pub async fn join(&self, token: &str, name: &str, report: Report, now: i64) -> Result<(Runner, Secret)> {
    check_name(name)?;
    let id = secret::id(secret::JOIN, token).ok_or(Error::InvalidJoinToken)?;
    let txn = self.db.begin().await?;
    let found = join_token::Entity::find_by_id(id).one(&txn).await?;
    let row = found.filter(|row| secret::matches(token, &row.hash)).ok_or(Error::InvalidJoinToken)?;
    if row.revoked_ms.is_some() {
      return Err(Error::Revoked(row.id));
    }
    if now >= row.expires_ms {
      return Err(Error::Expired(row.id));
    }
    if row.uses_left <= 0 {
      return Err(Error::UsedUp(row.id));
    }
    let scope = join_token_of(row.clone())?.scope;
    let (roles, labels) = admit(&scope, report)?;
    let existing = runner::Entity::find_by_id(name).one(&txn).await?;
    if existing.as_ref().is_some_and(|r| r.removed_ms.is_none()) {
      return Err(Error::RunnerExists(name.to_string()));
    }
    let mut spent = row.clone().into_active_model();
    spent.uses_left = Set(row.uses_left - 1);
    spent.update(&txn).await?;
    let credential = Secret::generate(secret::RUNNER, name);
    let joined = runner::Model {
      name: name.to_string(),
      roles: json(&roles)?,
      labels: json(&labels)?,
      scope_roles: json(&scope.roles)?,
      scope_labels: json(&scope.labels)?,
      join_token: row.id,
      credential_hash: Some(credential.hash()),
      pending_hash: None,
      rotating: false,
      created_ms: now,
      last_seen_ms: now,
      removed_ms: None,
    };
    // A removed runner's row gives way to the new one.
    if existing.is_some() {
      runner::Entity::delete_by_id(name).exec(&txn).await?;
    }
    runner::Entity::insert(joined.clone().into_active_model()).exec(&txn).await?;
    txn.commit().await?;
    Ok((runner_of(joined)?, credential))
  }

  /// The name of the runner `credential` belongs to, or `None` when no
  /// runner holds it. A rotation's new credential is accepted from its
  /// first use on, and that use retires the old one.
  pub async fn authenticate(&self, credential: &str) -> Result<Option<String>> {
    let Some(name) = secret::id(secret::RUNNER, credential) else {
      return Ok(None);
    };
    let Some(row) = runner::Entity::find_by_id(name).one(&self.db).await? else {
      return Ok(None);
    };
    let current = row.credential_hash.as_deref().is_some_and(|stored| secret::matches(credential, stored));
    let pending = row.pending_hash.as_deref().is_some_and(|stored| secret::matches(credential, stored));
    if pending {
      let mut confirmed = row.clone().into_active_model();
      confirmed.credential_hash = Set(row.pending_hash.clone());
      confirmed.pending_hash = Set(None);
      confirmed.rotating = Set(false);
      confirmed.update(&self.db).await?;
    }
    Ok((current || pending).then_some(row.name))
  }

  /// Records a heartbeat of the runner `name`: when it was seen and what it
  /// reports, within its join token's scope. Returns a new credential when
  /// a rotation is waiting for one; the runner's current credential stays
  /// valid until the new one is used.
  pub async fn heartbeat(&self, name: &str, report: Report, now: i64) -> Result<Option<Secret>> {
    let row = self.active(name).await?;
    let current = runner_of(row.clone())?;
    let (roles, labels) = admit(&current.scope, report)?;
    let mut beat = row.into_active_model();
    beat.roles = Set(json(&roles)?);
    beat.labels = Set(json(&labels)?);
    beat.last_seen_ms = Set(now);
    // A credential handed out before and not used yet is replaced: the
    // runner did not keep it.
    let credential = current.rotating.then(|| Secret::generate(secret::RUNNER, name));
    if let Some(credential) = &credential {
      beat.pending_hash = Set(Some(credential.hash()));
    }
    beat.update(&self.db).await?;
    Ok(credential)
  }

  /// Every runner, including removed ones, by name.
  pub async fn runners(&self) -> Result<Vec<Runner>> {
    let rows = runner::Entity::find().order_by_asc(runner::Column::Name).all(&self.db).await?;
    rows.into_iter().map(runner_of).collect()
  }

  /// Asks for a new credential for the runner `name`, which its next
  /// heartbeat receives.
  pub async fn rotate(&self, name: &str) -> Result<()> {
    let mut row = self.active(name).await?.into_active_model();
    row.rotating = Set(true);
    row.update(&self.db).await?;
    Ok(())
  }

  /// Revokes the runner `name`'s credentials. It stays listed as removed.
  pub async fn remove(&self, name: &str, now: i64) -> Result<()> {
    let mut row = self.active(name).await?.into_active_model();
    row.credential_hash = Set(None);
    row.pending_hash = Set(None);
    row.rotating = Set(false);
    row.removed_ms = Set(Some(now));
    row.update(&self.db).await?;
    Ok(())
  }

  /// The runner `name`, unless it does not exist or was removed.
  async fn active(&self, name: &str) -> Result<runner::Model> {
    let row = runner::Entity::find_by_id(name).filter(runner::Column::RemovedMs.is_null()).one(&self.db).await?;
    row.ok_or_else(|| Error::NoRunner(name.to_string()))
  }
}

/// Whether `token` is shaped like a runner credential, whether or not a
/// runner holds it.
pub fn is_runner_credential(token: &str) -> bool {
  secret::id(secret::RUNNER, token).is_some()
}

/// Whether the database holds migrations this store did not write.
async fn has_other_schema(db: &DatabaseConnection) -> Result<bool> {
  let manager = SchemaManager::new(db);
  if !manager.has_table("seaql_migrations").await? {
    return Ok(false);
  }
  let known: Vec<String> = migration::Migrator::migrations().iter().map(|m| m.name().to_string()).collect();
  let applied = sea_orm_migration::seaql_migrations::Entity::find().all(db).await?;
  Ok(applied.iter().any(|m| !known.contains(&m.version)))
}

/// The roles and labels a runner gets for `report` within `scope`: its own
/// roles, which the scope must allow, and its own labels with the scope's.
fn admit(scope: &Scope, report: Report) -> Result<(BTreeSet<RunnerRole>, BTreeMap<String, String>)> {
  if report.roles.is_empty() {
    return Err(Error::Invalid("a runner needs at least one role".into()));
  }
  if !scope.roles.is_empty()
    && let Some(role) = report.roles.iter().find(|role| !scope.roles.contains(role))
  {
    return Err(Error::OutOfScope(format!("the join token does not allow the role {role}")));
  }
  check_labels(&report.labels)?;
  let mut labels = report.labels;
  for (key, value) in &scope.labels {
    match labels.get(key) {
      Some(own) if own != value => {
        return Err(Error::OutOfScope(format!("the join token sets the label {key}={value}, not {key}={own}")));
      }
      _ => {
        labels.insert(key.clone(), value.clone());
      }
    }
  }
  Ok((report.roles, labels))
}

/// A runner's name is part of its credential and of `runner ls`, so it is
/// a DNS label: lowercase letters, digits and `-`.
fn check_name(name: &str) -> Result<()> {
  let allowed = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
  let valid = (1..=63).contains(&name.len()) && name.chars().all(allowed) && !name.starts_with('-') && !name.ends_with('-');
  if valid {
    return Ok(());
  }
  Err(Error::Invalid(format!(
    "invalid runner name {name:?}: use 1 to 63 lowercase letters, digits and '-', starting and ending with a letter or digit"
  )))
}

fn check_labels(labels: &BTreeMap<String, String>) -> Result<()> {
  let word = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
  for (key, value) in labels {
    if !(1..=63).contains(&key.len()) || !key.chars().all(|c| word(c) || c == '/') {
      return Err(Error::Invalid(format!("invalid label key {key:?}: use 1 to 63 letters, digits, '.', '_', '-' and '/'")));
    }
    if value.len() > 63 || !value.chars().all(word) {
      return Err(Error::Invalid(format!("invalid label value {value:?} of {key}: use up to 63 letters, digits, '.', '_' and '-'")));
    }
  }
  Ok(())
}

fn json(value: &impl Serialize) -> Result<String> {
  Ok(serde_json::to_string(value)?)
}

fn parse<T: DeserializeOwned>(text: &str) -> Result<T> {
  Ok(serde_json::from_str(text)?)
}

fn join_token_of(row: join_token::Model) -> Result<JoinToken> {
  Ok(JoinToken {
    scope: Scope {
      roles: parse(&row.roles)?,
      labels: parse(&row.labels)?,
    },
    id: row.id,
    created_ms: row.created_ms,
    expires_ms: row.expires_ms,
    uses_left: u32::try_from(row.uses_left).unwrap_or(0),
    revoked_ms: row.revoked_ms,
  })
}

fn runner_of(row: runner::Model) -> Result<Runner> {
  Ok(Runner {
    roles: parse(&row.roles)?,
    labels: parse(&row.labels)?,
    scope: Scope {
      roles: parse(&row.scope_roles)?,
      labels: parse(&row.scope_labels)?,
    },
    name: row.name,
    join_token: row.join_token,
    created_ms: row.created_ms,
    last_seen_ms: row.last_seen_ms,
    removed_ms: row.removed_ms,
    rotating: row.rotating,
  })
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
