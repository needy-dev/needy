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
