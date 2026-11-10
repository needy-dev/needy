//! `state.db`'s tables as SeaORM entities. Roles and labels are JSON text:
//! they are read and written whole, never queried by element.

pub mod runner {
  //! A runner that joined, including one that was removed.

  use sea_orm::entity::prelude::*;

  #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
  #[sea_orm(table_name = "runners")]
  pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub name: String,
    /// The roles the runner last reported, a JSON array.
    pub roles: String,
    /// The labels the runner last reported with those of its join token,
    /// a JSON object.
    pub labels: String,
    /// The roles its join token allowed, a JSON array; empty for any.
    pub scope_roles: String,
    /// The labels its join token set, a JSON object.
    pub scope_labels: String,
    /// The ID of the join token it joined with.
    pub join_token: String,
    /// The SHA-256 of its credential; `None` once removed.
    pub credential_hash: Option<Vec<u8>>,
    /// The SHA-256 of the credential a rotation handed out and the runner
    /// has not used yet.
    pub pending_hash: Option<Vec<u8>>,
    /// An operator asked for a new credential that the runner has not used
    /// yet.
    pub rotating: bool,
    pub created_ms: i64,
    pub last_seen_ms: i64,
    pub removed_ms: Option<i64>,
  }

  #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
  pub enum Relation {}

  impl ActiveModelBehavior for ActiveModel {}
}

pub mod join_token {
  //! A join token, including expired, spent and revoked ones.

  use sea_orm::entity::prelude::*;

  #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
  #[sea_orm(table_name = "join_tokens")]
  pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// The SHA-256 of the token.
    pub hash: Vec<u8>,
    /// The roles a runner may declare, a JSON array; empty for any.
    pub roles: String,
    /// The labels a runner gets, a JSON object.
    pub labels: String,
    pub created_ms: i64,
    pub expires_ms: i64,
    pub uses_left: i64,
    pub revoked_ms: Option<i64>,
  }

  #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
  pub enum Relation {}

  impl ActiveModelBehavior for ActiveModel {}
}

pub mod run {
  //! A run, live or ended, as the control plane last wrote it.

  use sea_orm::entity::prelude::*;

  #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
  #[sea_orm(table_name = "runs")]
  pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub name: String,
    /// `group` is an SQL keyword.
    pub group_name: String,
    pub profile_name: String,
    /// The profile the client resolved, as YAML.
    pub profile_yaml: String,
    /// `RunState` in snake case.
    pub state: String,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub actor_exit_code: Option<i32>,
    pub actor_exit_signal: Option<String>,
    pub failure: Option<String>,
    /// The runner the actor was placed on; `None` for the control host.
    pub actor_runner: Option<String>,
  }

  #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
  pub enum Relation {}

  impl ActiveModelBehavior for ActiveModel {}
}

pub mod instance {
  //! A run's hold on an actor or an executor instance. Runs that share a
  //! pooled instance have a row each.

  use sea_orm::entity::prelude::*;

  #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
  #[sea_orm(table_name = "instances")]
  pub struct Model {
    /// `RUN:actor` or `RUN:executor:NAME`.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub run: String,
    /// The executor's name in the profile, or `actor`.
    pub name: String,
    /// `actor` or `executor`.
    pub kind: String,
    /// The runner that owns it; `None` for the control host.
    pub runner: Option<String>,
    /// `firecracker`, `native` or `docker`.
    pub provider: String,
    /// The executor's runtime as `run ls` shows it: `native`, `docker`,
    /// `runner`; `firecracker` for an actor.
    pub runtime: String,
    /// The container's name, the runner's name, `local`, or the VM's
    /// directory.
    pub handle: String,
    /// The VM's Firecracker process.
    pub pid: Option<i64>,
    /// `running`, `released`, `ended` or `lost`.
    pub state: String,
    pub created_ms: i64,
    pub ended_ms: Option<i64>,
  }

  #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
  pub enum Relation {}

  impl ActiveModelBehavior for ActiveModel {}
}

pub mod process {
  //! A process a run's executor ran.

  use sea_orm::entity::prelude::*;

  #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
  #[sea_orm(table_name = "processes")]
  pub struct Model {
    /// The process ID its executor gave it.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub run: String,
    /// The instance row it runs on.
    pub instance: String,
    /// `Origin` in kebab case; `None` for a process a runner reported that
    /// this control plane did not start.
    pub origin: Option<String>,
    /// `running`, `exited`, `ended` or `lost`.
    pub state: String,
    pub exit_code: Option<i32>,
    pub exit_signal: Option<String>,
    pub started_ms: i64,
    pub ended_ms: Option<i64>,
  }

  #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
  pub enum Relation {}

  impl ActiveModelBehavior for ActiveModel {}
}

pub mod client {
  //! A paired client, including revoked ones.

  use sea_orm::entity::prelude::*;

  #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
  #[sea_orm(table_name = "clients")]
  pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// What the trace names it: `client:NAME`.
    pub name: String,
    /// The ID of the pairing code it paired with.
    pub pairing_code: String,
    /// The SHA-256 of its token; `None` once revoked.
    pub token_hash: Option<Vec<u8>>,
    pub created_ms: i64,
    /// Its last request, written at most once a minute.
    pub last_used_ms: Option<i64>,
    pub revoked_ms: Option<i64>,
  }

  #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
  pub enum Relation {}

  impl ActiveModelBehavior for ActiveModel {}
}

pub mod pairing_code {
  //! A pairing code, including expired and used ones.

  use sea_orm::entity::prelude::*;

  #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
  #[sea_orm(table_name = "pairing_codes")]
  pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// The SHA-256 of the code.
    pub hash: Vec<u8>,
    /// The name of the client that pairs with it.
    pub name: String,
    pub created_ms: i64,
    pub expires_ms: i64,
    /// When a client paired with it; a code is used once.
    pub used_ms: Option<i64>,
  }

  #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
  pub enum Relation {}

  impl ActiveModelBehavior for ActiveModel {}
}
