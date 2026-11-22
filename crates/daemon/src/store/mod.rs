//! `state.db`: the control plane's own state, separate from `trace.db`.
//! It holds the runner registry (join tokens and the runners that joined
//! with them, [`runners`]), the client registry (pairing codes and the clients
//! that paired with them, [`clients`]) and runs with the instances and
//! processes they hold ([`runs`]).
//!
//! Secrets are stored as hashes only ([`secret`]). Every time is passed in
//! by the caller in Unix milliseconds, so expiry is decided by one clock.
//!
//! Schema changes go through [`migration`]; do not issue DDL anywhere else.

mod client_execution;
mod clients;
mod entity;
pub mod lifecycle;
mod migration;
mod runners;
mod runs;
mod secret;

use std::path::{Path, PathBuf};

use sea_orm::{ConnectOptions, ConnectionTrait as _, Database, DatabaseConnection, DbErr, EntityTrait as _};
use sea_orm_migration::{MigratorTrait as _, SchemaManager};
use serde::Serialize;
use serde::de::DeserializeOwned;

pub use self::clients::{Client, is_client_token};
pub use self::lifecycle::{InstanceState, ProcessState, Refused};
pub use self::runners::{JoinToken, NewJoinToken, Report, Runner, Scope, is_runner_credential};
pub use self::runs::{InstanceKind, InstanceRecord, ProcessRecord, RunRecord, actor_instance, executor_instance};
pub use self::secret::Secret;
pub(crate) use self::secret::hex;

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("{0} was written by another version of needy; inspect its schema and use a compatible version or explicit migration")]
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
  /// Malformed, unknown, or with a wrong secret: which of these is not
  /// said, so a guess learns nothing.
  #[error("the pairing code is not valid")]
  InvalidPairingCode,
  #[error("pairing code {0} expired")]
  PairingCodeExpired(String),
  #[error("pairing code {0} was used already")]
  PairingCodeUsed(String),
  #[error("a pairing code for client {0} is open; pair with it or wait until it expires")]
  PairingCodeOpen(String),
  #[error("no client {0}")]
  NoClient(String),
  /// A malformed name, label or limit.
  #[error("{0}")]
  Invalid(String),
  /// A change of state its lifecycle does not allow ([`lifecycle`]).
  #[error(transparent)]
  Refused(#[from] Refused),
  #[error("a row of state.db cannot be read")]
  Corrupt(#[from] serde_json::Error),
  #[error(transparent)]
  Database(#[from] DbErr),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

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

/// A name that is a DNS label: lowercase letters, digits and `-`.
fn check_label(what: &str, name: &str) -> Result<()> {
  let allowed = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
  let valid = (1..=63).contains(&name.len()) && name.chars().all(allowed) && !name.starts_with('-') && !name.ends_with('-');
  if valid {
    return Ok(());
  }
  Err(Error::Invalid(format!(
    "invalid {what} name {name:?}: use 1 to 63 lowercase letters, digits and '-', starting and ending with a letter or digit"
  )))
}

fn json(value: &impl Serialize) -> Result<String> {
  Ok(serde_json::to_string(value)?)
}

fn parse<T: DeserializeOwned>(text: &str) -> Result<T> {
  Ok(serde_json::from_str(text)?)
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
