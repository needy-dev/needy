//! The daemon's SQLite database, through SeaORM.
//!
//! Schema changes go through `migration`; do not issue DDL anywhere else.

pub mod event;
pub mod migration;
pub mod session;

use std::path::Path;

use anyhow::{Context, Result};
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;

/// Opens (creating if needed) the database at `path` and applies pending
/// migrations.
pub async fn open(path: &Path) -> Result<DatabaseConnection> {
  let url = format!("sqlite://{}?mode=rwc", path.display());
  let mut options = ConnectOptions::new(url);
  options.sqlx_logging(false);
  let db = Database::connect(options).await.with_context(|| format!("open {}", path.display()))?;
  // NOTICE: WAL lets `needy report` read while the daemon writes. It is a
  // property of the database file, so setting it once per connection is
  // enough.
  db.execute_unprepared("PRAGMA journal_mode=WAL").await.context("enable WAL")?;
  migration::Migrator::up(&db, None).await.context("migrate database")?;
  Ok(db)
}
