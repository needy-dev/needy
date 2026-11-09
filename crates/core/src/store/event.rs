//! One trace event.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "events")]
pub struct Model {
  #[sea_orm(primary_key)]
  pub id: i64,
  pub session: String,
  pub time_ms: i64,
  /// The event kind, for example `exec.decide`.
  pub kind: String,
  /// The payload schema version (`core::event::SCHEMA_VERSION`); 0 marks
  /// rows written before the typed schema.
  pub version: i32,
  /// The kind's payload as a JSON object.
  pub payload: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
