//! Schema migrations, applied in order by `store::open`.

use sea_orm_migration::prelude::*;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
  fn migrations() -> Vec<Box<dyn MigrationTrait>> {
    vec![Box::new(CreateTrace)]
  }
}

/// The `events` and `sessions` tables.
///
/// NOTICE: databases written before migrations existed already have both
/// tables with this exact shape and no migration table, so every statement
/// uses `IF NOT EXISTS` and the migration adopts them unchanged.
struct CreateTrace;

// NOTICE: `DeriveMigrationName` names a migration after its module, which
// would give every migration in this file the same name.
impl MigrationName for CreateTrace {
  fn name(&self) -> &str {
    "m20261109_000001_create_trace"
  }
}

#[derive(DeriveIden)]
enum Events {
  Table,
  Id,
  Session,
  TimeMs,
  Kind,
  Attrs,
}

#[derive(DeriveIden)]
enum Sessions {
  Table,
  Id,
  Name,
  Grp,
  Profile,
  CreatedMs,
  EndedMs,
  State,
}

#[async_trait::async_trait]
impl MigrationTrait for CreateTrace {
  async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager
      .create_table(
        Table::create()
          .table(Events::Table)
          .if_not_exists()
          .col(ColumnDef::new(Events::Id).integer().not_null().auto_increment().primary_key())
          .col(ColumnDef::new(Events::Session).text().not_null())
          .col(ColumnDef::new(Events::TimeMs).big_integer().not_null())
          .col(ColumnDef::new(Events::Kind).text().not_null())
          .col(ColumnDef::new(Events::Attrs).text().not_null())
          .to_owned(),
      )
      .await?;
    manager
      .create_index(
        Index::create().name("events_session").table(Events::Table).col(Events::Session).col(Events::Id).if_not_exists().to_owned(),
      )
      .await?;
    manager
      .create_table(
        Table::create()
          .table(Sessions::Table)
          .if_not_exists()
          .col(ColumnDef::new(Sessions::Id).text().not_null().primary_key())
          .col(ColumnDef::new(Sessions::Name).text())
          .col(ColumnDef::new(Sessions::Grp).text())
          .col(ColumnDef::new(Sessions::Profile).text())
          .col(ColumnDef::new(Sessions::CreatedMs).big_integer())
          .col(ColumnDef::new(Sessions::EndedMs).big_integer())
          .col(ColumnDef::new(Sessions::State).text())
          .to_owned(),
      )
      .await
  }

  async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager.drop_table(Table::drop().table(Sessions::Table).to_owned()).await?;
    manager.drop_table(Table::drop().table(Events::Table).to_owned()).await
  }
}
