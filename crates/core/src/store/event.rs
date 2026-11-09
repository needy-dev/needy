//! One trace event.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "events")]
pub struct Model {
  #[sea_orm(primary_key)]
  pub id: i64,
  pub session: String,
  pub time_ms: i64,
  pub kind: String,
  /// A JSON object of string attributes.
  pub attrs: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
