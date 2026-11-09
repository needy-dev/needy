//! One session, derived from its `session.*` and `actor.exit` trace events.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "sessions")]
pub struct Model {
  #[sea_orm(primary_key, auto_increment = false)]
  pub id: String,
  pub name: Option<String>,
  /// The session's group. `group` is an SQL keyword.
  pub grp: Option<String>,
  pub profile: Option<String>,
  pub created_ms: Option<i64>,
  pub ended_ms: Option<i64>,
  /// A `core::types::SessionState`: `running`, `stopped` or `finished`.
  pub state: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
