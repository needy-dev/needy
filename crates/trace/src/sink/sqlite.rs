//! The SQLite sink and its reader, through SeaORM. `needy report` reads a
//! session's events back with [`load`].
//!
//! Schema changes go through [`migration`]; do not issue DDL anywhere else.

use std::path::{Path, PathBuf};

use sea_orm::sea_query::Expr;
use sea_orm::{
  ActiveValue::Set, ColumnTrait, ConnectOptions, ConnectionTrait, Database, DatabaseConnection, EntityTrait, Order, QueryFilter, QueryOrder,
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

  /// A sink that writes one session's events.
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

/// Writes the events of one session.
pub struct Sqlite {
  db: DatabaseConnection,
  /// Whether this sink has added its session to `sessions`.
  registered: bool,
}

#[async_trait::async_trait]
impl Sink for Sqlite {
  async fn write(&mut self, event: &Event) -> Result<(), Error> {
    if !self.registered {
      let row = entity::session::ActiveModel {
        id: Set(event.session.clone()),
        first_ms: Set(event.time_ms),
      };
      entity::session::Entity::insert(row).on_conflict_do_nothing_on([entity::session::Column::Id]).exec(&self.db).await?;
      self.registered = true;
    }
    entity::event::Entity::insert(entity::event::ActiveModel {
      session: Set(event.session.clone()),
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

/// One session's events, read back.
pub struct Loaded {
  /// The full session ID.
  pub session: String,
  /// In the order of their `seq`.
  pub events: Vec<Event>,
}

/// Reads the events of `session`, an ID prefix or `latest`. A prefix that
/// matches several sessions picks the one that started last.
pub async fn load(path: &Path, session: &str) -> Result<Loaded, Error> {
  if !path.exists() {
    return Err(Error::NoDatabase(PathBuf::from(path)));
  }
  let store = Store::open(path).await?;
  let mut query = entity::session::Entity::find()
    .order_by_desc(entity::session::Column::FirstMs)
    // NOTICE: sessions that started in the same millisecond keep the order
    // they were added in through SQLite's implicit `rowid`.
    .order_by(Expr::cust("rowid"), Order::Desc);
  if session != "latest" {
    query = query.filter(entity::session::Column::Id.starts_with(session));
  }
  let found = query.one(&store.db).await?.ok_or_else(|| Error::NoSession(session.to_string()))?;
  let rows = entity::event::Entity::find()
    .filter(entity::event::Column::Session.eq(&found.id))
    .order_by_asc(entity::event::Column::Seq)
    .order_by_asc(entity::event::Column::Id)
    .all(&store.db)
    .await?;
  let events = rows
    .into_iter()
    .map(|row| {
      Ok(Event {
        session: row.session,
        seq: u64::try_from(row.seq).unwrap_or(0),
        time_ms: row.time_ms,
        kind: row.kind,
        version: u32::try_from(row.version).unwrap_or(0),
        payload: RawValue::from_string(row.payload)?,
      })
    })
    .collect::<Result<_, Error>>()?;
  Ok(Loaded {
    session: found.id,
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
      pub session: String,
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

  pub mod session {
    //! A session with events, and the time of its first one; readers use it
    //! to resolve ID prefixes and `latest`.

    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "sessions")]
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

  /// The `events` and `sessions` tables of the event envelope.
  struct CreateTrace;

  // NOTICE: `DeriveMigrationName` names a migration after its module, which
  // would give every migration in this module the same name. The name differs
  // from the migrations `needy-core` applied before the trace crate, so their
  // databases are recognized and refused (principle 7: no migration path).
  impl MigrationName for CreateTrace {
    fn name(&self) -> &str {
      "m20261109_000010_trace_envelope"
    }
  }

  #[derive(DeriveIden)]
  enum Events {
    Table,
    Id,
    Session,
    Seq,
    TimeMs,
    Kind,
    Version,
    Payload,
  }

  #[derive(DeriveIden)]
  enum Sessions {
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
            .col(ColumnDef::new(Events::Session).text().not_null())
            .col(ColumnDef::new(Events::Seq).big_integer().not_null())
            .col(ColumnDef::new(Events::TimeMs).big_integer().not_null())
            .col(ColumnDef::new(Events::Kind).text().not_null())
            .col(ColumnDef::new(Events::Version).integer().not_null())
            .col(ColumnDef::new(Events::Payload).text().not_null())
            .to_owned(),
        )
        .await?;
      manager
        .create_index(Index::create().name("events_session_seq").table(Events::Table).col(Events::Session).col(Events::Seq).to_owned())
        .await?;
      manager
        .create_table(
          Table::create()
            .table(Sessions::Table)
            .col(ColumnDef::new(Sessions::Id).text().not_null().primary_key())
            .col(ColumnDef::new(Sessions::FirstMs).big_integer().not_null())
            .to_owned(),
        )
        .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
      manager.drop_table(Table::drop().table(Sessions::Table).to_owned()).await?;
      manager.drop_table(Table::drop().table(Events::Table).to_owned()).await
    }
  }
}

#[cfg(test)]
#[path = "sqlite_test.rs"]
mod tests;
