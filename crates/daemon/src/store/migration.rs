//! Schema migrations of `state.db`, applied in order by
//! [`super::Store::open`]. Do not issue DDL anywhere else.

use sea_orm_migration::prelude::*;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
  fn migrations() -> Vec<Box<dyn MigrationTrait>> {
    vec![
      Box::new(CreateRunners),
      Box::new(CreateRuns),
      Box::new(CreateClients),
    ]
  }
}

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

/// The `runs`, `instances` and `processes` tables.
struct CreateRuns;

impl MigrationName for CreateRuns {
  fn name(&self) -> &str {
    "m20261110_000060_runs"
  }
}

#[derive(DeriveIden)]
enum Runs {
  Table,
  Id,
  Name,
  GroupName,
  ProfileName,
  ProfileYaml,
  State,
  CreatedMs,
  UpdatedMs,
  ActorExitCode,
  ActorExitSignal,
  Failure,
  ActorRunner,
}

#[derive(DeriveIden)]
enum Instances {
  Table,
  Id,
  Run,
  Name,
  Kind,
  Runner,
  Provider,
  Runtime,
  Handle,
  Pid,
  State,
  CreatedMs,
  EndedMs,
}

#[derive(DeriveIden)]
enum Processes {
  Table,
  Id,
  Run,
  Instance,
  Origin,
  State,
  ExitCode,
  ExitSignal,
  StartedMs,
  EndedMs,
}

#[async_trait::async_trait]
impl MigrationTrait for CreateRuns {
  async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager
      .create_table(
        Table::create()
          .table(Runs::Table)
          .col(ColumnDef::new(Runs::Id).text().not_null().primary_key())
          .col(ColumnDef::new(Runs::Name).text().not_null())
          .col(ColumnDef::new(Runs::GroupName).text().not_null())
          .col(ColumnDef::new(Runs::ProfileName).text().not_null())
          .col(ColumnDef::new(Runs::ProfileYaml).text().not_null())
          .col(ColumnDef::new(Runs::State).text().not_null())
          .col(ColumnDef::new(Runs::CreatedMs).big_integer().not_null())
          .col(ColumnDef::new(Runs::UpdatedMs).big_integer().not_null())
          .col(ColumnDef::new(Runs::ActorExitCode).integer())
          .col(ColumnDef::new(Runs::ActorExitSignal).text())
          .col(ColumnDef::new(Runs::Failure).text())
          .col(ColumnDef::new(Runs::ActorRunner).text())
          .to_owned(),
      )
      .await?;
    manager
      .create_table(
        Table::create()
          .table(Instances::Table)
          .col(ColumnDef::new(Instances::Id).text().not_null().primary_key())
          .col(ColumnDef::new(Instances::Run).text().not_null())
          .col(ColumnDef::new(Instances::Name).text().not_null())
          .col(ColumnDef::new(Instances::Kind).text().not_null())
          .col(ColumnDef::new(Instances::Runner).text())
          .col(ColumnDef::new(Instances::Provider).text().not_null())
          .col(ColumnDef::new(Instances::Runtime).text().not_null())
          .col(ColumnDef::new(Instances::Handle).text().not_null())
          .col(ColumnDef::new(Instances::Pid).big_integer())
          .col(ColumnDef::new(Instances::State).text().not_null())
          .col(ColumnDef::new(Instances::CreatedMs).big_integer().not_null())
          .col(ColumnDef::new(Instances::EndedMs).big_integer())
          .to_owned(),
      )
      .await?;
    manager.create_index(Index::create().name("instances_run").table(Instances::Table).col(Instances::Run).to_owned()).await?;
    manager.create_index(Index::create().name("instances_state").table(Instances::Table).col(Instances::State).to_owned()).await?;
    manager
      .create_table(
        Table::create()
          .table(Processes::Table)
          .col(ColumnDef::new(Processes::Id).text().not_null().primary_key())
          .col(ColumnDef::new(Processes::Run).text().not_null())
          .col(ColumnDef::new(Processes::Instance).text().not_null())
          .col(ColumnDef::new(Processes::Origin).text())
          .col(ColumnDef::new(Processes::State).text().not_null())
          .col(ColumnDef::new(Processes::ExitCode).integer())
          .col(ColumnDef::new(Processes::ExitSignal).text())
          .col(ColumnDef::new(Processes::StartedMs).big_integer().not_null())
          .col(ColumnDef::new(Processes::EndedMs).big_integer())
          .to_owned(),
      )
      .await?;
    manager.create_index(Index::create().name("processes_run").table(Processes::Table).col(Processes::Run).to_owned()).await
  }

  async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager.drop_table(Table::drop().table(Processes::Table).to_owned()).await?;
    manager.drop_table(Table::drop().table(Instances::Table).to_owned()).await?;
    manager.drop_table(Table::drop().table(Runs::Table).to_owned()).await
  }
}

/// The `clients` and `pairing_codes` tables.
struct CreateClients;

impl MigrationName for CreateClients {
  fn name(&self) -> &str {
    "m20261110_000070_clients"
  }
}

#[derive(DeriveIden)]
enum Clients {
  Table,
  Id,
  Name,
  PairingCode,
  TokenHash,
  CreatedMs,
  LastUsedMs,
  RevokedMs,
}

#[derive(DeriveIden)]
enum PairingCodes {
  Table,
  Id,
  Hash,
  Name,
  CreatedMs,
  ExpiresMs,
  UsedMs,
}

#[async_trait::async_trait]
impl MigrationTrait for CreateClients {
  async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager
      .create_table(
        Table::create()
          .table(Clients::Table)
          .col(ColumnDef::new(Clients::Id).text().not_null().primary_key())
          .col(ColumnDef::new(Clients::Name).text().not_null())
          .col(ColumnDef::new(Clients::PairingCode).text().not_null())
          .col(ColumnDef::new(Clients::TokenHash).blob())
          .col(ColumnDef::new(Clients::CreatedMs).big_integer().not_null())
          .col(ColumnDef::new(Clients::LastUsedMs).big_integer())
          .col(ColumnDef::new(Clients::RevokedMs).big_integer())
          .to_owned(),
      )
      .await?;
    manager
      .create_table(
        Table::create()
          .table(PairingCodes::Table)
          .col(ColumnDef::new(PairingCodes::Id).text().not_null().primary_key())
          .col(ColumnDef::new(PairingCodes::Hash).blob().not_null())
          .col(ColumnDef::new(PairingCodes::Name).text().not_null())
          .col(ColumnDef::new(PairingCodes::CreatedMs).big_integer().not_null())
          .col(ColumnDef::new(PairingCodes::ExpiresMs).big_integer().not_null())
          .col(ColumnDef::new(PairingCodes::UsedMs).big_integer())
          .to_owned(),
      )
      .await
  }

  async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager.drop_table(Table::drop().table(PairingCodes::Table).to_owned()).await?;
    manager.drop_table(Table::drop().table(Clients::Table).to_owned()).await
  }
}
