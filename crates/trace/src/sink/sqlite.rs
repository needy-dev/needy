//! The SQLite sink and its reader, through SeaORM. `needy report` reads a
//! run's events back with [`load`].
//!
//! Schema changes go through [`migration`]; do not issue DDL anywhere else.

use std::path::{Path, PathBuf};

use sea_orm::sea_query::Expr;
use sea_orm::{
  ActiveValue::Set, ColumnTrait, ConnectOptions, ConnectionTrait, Database, DatabaseConnection, EntityTrait, Order, QueryFilter, QueryOrder,
  QuerySelect,
};
use sea_orm_migration::{MigratorTrait, SchemaManager};
use serde_json::value::RawValue;

use crate::{Error, Event, Sink};

/// An open trace database. Clones share the connection pool.
#[derive(Clone)]
pub struct Store {
  db: DatabaseConnection,
}

impl Store {
  /// Opens, creating if needed, the database at `path` and applies pending
  /// migrations. A database another schema wrote is refused, not migrated.
  pub async fn open(path: &Path) -> Result<Self, Error> {
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let mut options = ConnectOptions::new(url);
    options.sqlx_logging(false);
    let db = Database::connect(options).await?;
    // NOTICE: WAL lets `needy report` read while the daemon writes. It is a
    // property of the database file, so setting it once per connection is
    // enough.
    db.execute_unprepared("PRAGMA journal_mode=WAL").await?;
    if has_other_schema(&db).await? {
      return Err(Error::Schema(path.to_path_buf()));
    }
    migration::Migrator::up(&db, None).await?;
    Ok(Self { db })
  }

  /// Opens the database at `path` read-only, for a reader such as `needy
  /// report`, and checks its schema before anything else. Nothing is
  /// created, migrated or switched to WAL: a reader never changes the file.
  async fn read(path: &Path) -> Result<Self, Error> {
    // NOTICE: `mode=ro` rather than `immutable=1`: the daemon may be writing
    // the database while a report reads it, which `immutable` would not
    // see (`https://www.sqlite.org/uri.html#uriimmutable`).
    let url = format!("sqlite://{}?mode=ro", path.display());
    let mut options = ConnectOptions::new(url);
    options.sqlx_logging(false);
    let db = Database::connect(options).await?;
    if has_other_schema(&db).await? {
      return Err(Error::Schema(path.to_path_buf()));
    }
    Ok(Self { db })
  }

  /// Whether this crate's migrations created the tables; an empty
  /// database has none and no runs.
  async fn has_trace(&self) -> Result<bool, Error> {
    let manager = SchemaManager::new(&self.db);
    Ok(manager.has_table("seaql_migrations").await? && manager.has_table("runs").await?)
  }

  /// A sink that writes one run's events.
  pub fn sink(&self) -> Sqlite {
    Sqlite {
      db: self.db.clone(),
      registered: false,
    }
  }
}

/// Whether the database holds tables or migrations this crate did not write.
async fn has_other_schema(db: &DatabaseConnection) -> Result<bool, Error> {
  let manager = SchemaManager::new(db);
  if !manager.has_table("seaql_migrations").await? {
    // Before migrations existed, the daemon created `events` by hand.
    return Ok(manager.has_table("events").await?);
  }
  let known: Vec<String> = migration::Migrator::migrations().iter().map(|m| m.name().to_string()).collect();
  let applied = sea_orm_migration::seaql_migrations::Entity::find().all(db).await?;
  Ok(applied.iter().any(|m| !known.contains(&m.version)))
}

/// Writes the events of one run.
pub struct Sqlite {
  db: DatabaseConnection,
  /// Whether this sink has added its run to `runs`.
  registered: bool,
}

#[async_trait::async_trait]
impl Sink for Sqlite {
  async fn write(&mut self, event: &Event) -> Result<(), Error> {
    if !self.registered {
      let row = entity::run::ActiveModel {
        id: Set(event.run.clone()),
        first_ms: Set(event.time_ms),
      };
      entity::run::Entity::insert(row).on_conflict_do_nothing_on([entity::run::Column::Id]).exec(&self.db).await?;
      self.registered = true;
    }
    entity::event::Entity::insert(entity::event::ActiveModel {
      run: Set(event.run.clone()),
      seq: Set(i64::try_from(event.seq).unwrap_or(i64::MAX)),
      time_ms: Set(event.time_ms),
      kind: Set(event.kind.clone()),
      version: Set(i64::from(event.version)),
      payload: Set(event.payload.get().to_string()),
      ..Default::default()
    })
    .exec(&self.db)
    .await?;
    Ok(())
  }
}

/// One run's events, read back.
pub struct Loaded {
  /// The full run ID.
  pub run: String,
  /// In the order of their `seq`.
  pub events: Vec<Event>,
}

/// Reads a full ID, a unique ID prefix or `latest`. An ambiguous prefix is
/// rejected rather than silently choosing the newest run.
// NOTICE: the CLI configuration/access redesign changes prefix selection;
// exact IDs and explicit `latest` keep their previous meaning.
pub async fn load(path: &Path, run: &str) -> Result<Loaded, Error> {
  if !path.exists() {
    return Err(Error::NoDatabase(PathBuf::from(path)));
  }
  let store = Store::read(path).await?;
  if !store.has_trace().await? {
    return Err(Error::NoRun(run.to_string()));
  }
  let query = entity::run::Entity::find()
    .order_by_desc(entity::run::Column::FirstMs)
    // NOTICE: runs that started in the same millisecond keep the order
    // they were added in through SQLite's implicit `rowid`.
    .order_by(Expr::cust("rowid"), Order::Desc);
  let found = if run == "latest" {
    query.one(&store.db).await?.ok_or_else(|| Error::NoRun(run.to_string()))?
  } else if let Some(exact) = entity::run::Entity::find_by_id(run).one(&store.db).await? {
    exact
  } else {
    let matches = query.filter(entity::run::Column::Id.starts_with(run)).limit(2).all(&store.db).await?;
    match matches.len() {
      0 => return Err(Error::NoRun(run.to_string())),
      1 => matches.into_iter().next().expect("one run"),
      _ => {
        return Err(Error::AmbiguousRun {
          prefix: run.to_string(),
          candidates: matches.into_iter().map(|run| run.id).collect(),
        });
      }
    }
  };
  let rows = entity::event::Entity::find()
    .filter(entity::event::Column::Run.eq(&found.id))
    .order_by_asc(entity::event::Column::Seq)
    .order_by_asc(entity::event::Column::Id)
    .all(&store.db)
    .await?;
  let events = rows
    .into_iter()
    .map(|row| {
      Ok(Event {
        run: row.run,
        seq: u64::try_from(row.seq).unwrap_or(0),
        time_ms: row.time_ms,
        kind: row.kind,
        version: u32::try_from(row.version).unwrap_or(0),
        payload: RawValue::from_string(row.payload)?,
      })
    })
    .collect::<Result<_, Error>>()?;
  Ok(Loaded {
    run: found.id,
    events,
  })
}

mod entity {
  pub mod event {
    //! One event envelope.

    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "events")]
    pub struct Model {
      #[sea_orm(primary_key)]
      pub id: i64,
      pub run: String,
      pub seq: i64,
      pub time_ms: i64,
      pub kind: String,
      pub version: i64,
      /// A JSON object.
      pub payload: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
  }

  pub mod run {
    //! A run with events, and the time of its first one; readers use it
    //! to resolve ID prefixes and `latest`.

    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "runs")]
    pub struct Model {
      #[sea_orm(primary_key, auto_increment = false)]
      pub id: String,
      pub first_ms: i64,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
  }
}

pub mod migration {
  //! Schema migrations, applied in order by [`super::Store::open`].

  use sea_orm_migration::prelude::*;

  pub struct Migrator;

  #[async_trait::async_trait]
  impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
      vec![Box::new(CreateTrace)]
    }
  }

  /// The `events` and `runs` tables of the event envelope.
  struct CreateTrace;

  // NOTICE: `DeriveMigrationName` names a migration after its module, which
  // would give every migration in this module the same name. The name differs
  // from the migrations `needy-core` applied before the trace crate and from
  // `m20261109_000010_trace_envelope`, the same tables with `session` for
  // `run`, so their databases are recognized and refused (principle 7: no
  // migration path).
  impl MigrationName for CreateTrace {
    fn name(&self) -> &str {
      "m20261110_000020_trace_runs"
    }
  }

  #[derive(DeriveIden)]
  enum Events {
    Table,
    Id,
    Run,
    Seq,
    TimeMs,
    Kind,
    Version,
    Payload,
  }

  #[derive(DeriveIden)]
  enum Runs {
    Table,
    Id,
    FirstMs,
  }

  #[async_trait::async_trait]
  impl MigrationTrait for CreateTrace {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
      manager
        .create_table(
          Table::create()
            .table(Events::Table)
            .col(ColumnDef::new(Events::Id).integer().not_null().auto_increment().primary_key())
            .col(ColumnDef::new(Events::Run).text().not_null())
            .col(ColumnDef::new(Events::Seq).big_integer().not_null())
            .col(ColumnDef::new(Events::TimeMs).big_integer().not_null())
            .col(ColumnDef::new(Events::Kind).text().not_null())
            .col(ColumnDef::new(Events::Version).integer().not_null())
            .col(ColumnDef::new(Events::Payload).text().not_null())
            .to_owned(),
        )
        .await?;
      manager.create_index(Index::create().name("events_run_seq").table(Events::Table).col(Events::Run).col(Events::Seq).to_owned()).await?;
      manager
        .create_table(
          Table::create()
            .table(Runs::Table)
            .col(ColumnDef::new(Runs::Id).text().not_null().primary_key())
            .col(ColumnDef::new(Runs::FirstMs).big_integer().not_null())
            .to_owned(),
        )
        .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
      manager.drop_table(Table::drop().table(Runs::Table).to_owned()).await?;
      manager.drop_table(Table::drop().table(Events::Table).to_owned()).await
    }
  }
}

#[cfg(test)]
#[path = "sqlite_test.rs"]
mod tests;
