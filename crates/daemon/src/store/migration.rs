//! Schema migrations of `state.db`, applied in order by
//! [`super::Store::open`]. Do not issue DDL anywhere else.

use sea_orm_migration::prelude::*;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
  fn migrations() -> Vec<Box<dyn MigrationTrait>> {
    vec![Box::new(CreateRunners)]
  }
}

// TODO(persistence): runs, instances, processes and clients join `state.db`
// in slices 6 and 7 of the runners-and-runs design
// (`docs/ai/references/architecture/2026-11-10-runner-and-run-design.md`).

/// The `runners` and `join_tokens` tables.
struct CreateRunners;

// NOTICE: `DeriveMigrationName` names a migration after its module, which
// would give every migration in this module the same name.
impl MigrationName for CreateRunners {
  fn name(&self) -> &str {
    "m20261110_000030_runners"
  }
}

#[derive(DeriveIden)]
enum Runners {
  Table,
  Name,
  Roles,
  Labels,
  ScopeRoles,
  ScopeLabels,
  JoinToken,
  CredentialHash,
  PendingHash,
  Rotating,
  CreatedMs,
  LastSeenMs,
  RemovedMs,
}

#[derive(DeriveIden)]
enum JoinTokens {
  Table,
  Id,
  Hash,
  Roles,
  Labels,
  CreatedMs,
  ExpiresMs,
  UsesLeft,
  RevokedMs,
}

#[async_trait::async_trait]
impl MigrationTrait for CreateRunners {
  async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager
      .create_table(
        Table::create()
          .table(Runners::Table)
          .col(ColumnDef::new(Runners::Name).text().not_null().primary_key())
          .col(ColumnDef::new(Runners::Roles).text().not_null())
          .col(ColumnDef::new(Runners::Labels).text().not_null())
          .col(ColumnDef::new(Runners::ScopeRoles).text().not_null())
          .col(ColumnDef::new(Runners::ScopeLabels).text().not_null())
          .col(ColumnDef::new(Runners::JoinToken).text().not_null())
          .col(ColumnDef::new(Runners::CredentialHash).blob())
          .col(ColumnDef::new(Runners::PendingHash).blob())
          .col(ColumnDef::new(Runners::Rotating).boolean().not_null())
          .col(ColumnDef::new(Runners::CreatedMs).big_integer().not_null())
          .col(ColumnDef::new(Runners::LastSeenMs).big_integer().not_null())
          .col(ColumnDef::new(Runners::RemovedMs).big_integer())
          .to_owned(),
      )
      .await?;
    manager
      .create_table(
        Table::create()
          .table(JoinTokens::Table)
          .col(ColumnDef::new(JoinTokens::Id).text().not_null().primary_key())
          .col(ColumnDef::new(JoinTokens::Hash).blob().not_null())
          .col(ColumnDef::new(JoinTokens::Roles).text().not_null())
          .col(ColumnDef::new(JoinTokens::Labels).text().not_null())
          .col(ColumnDef::new(JoinTokens::CreatedMs).big_integer().not_null())
          .col(ColumnDef::new(JoinTokens::ExpiresMs).big_integer().not_null())
          .col(ColumnDef::new(JoinTokens::UsesLeft).big_integer().not_null())
          .col(ColumnDef::new(JoinTokens::RevokedMs).big_integer())
          .to_owned(),
      )
      .await
  }

  async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager.drop_table(Table::drop().table(JoinTokens::Table).to_owned()).await?;
    manager.drop_table(Table::drop().table(Runners::Table).to_owned()).await
  }
}
