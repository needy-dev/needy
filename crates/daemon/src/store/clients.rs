//! The client registry: pairing codes and the clients that paired with
//! them.
//!
//! An operator creates a pairing code (short-lived, single-use, shown
//! once); a client exchanges it for its own token, which it presents as its
//! bearer token from then on. Both are stored as hashes only, like join
//! tokens and runner credentials ([`super::secret`]). A client's name is
//! what the trace records it as (`client:NAME`), so two clients that are
//! not revoked never share one.
//!
//! The name is also the client's identity across pairings (owner decision,
//! 2026-11-12): a pairing code for the name of a client that is not
//! revoked re-pairs that client, and pairing with it revokes the client's
//! old token in the same transaction.

use needy_api_server::control::ClientAccess;

use sea_orm::{
  ActiveModelTrait as _, ActiveValue::Set, ColumnTrait as _, Condition, ConnectionTrait, EntityTrait as _, IntoActiveModel as _,
  QueryFilter as _, QueryOrder as _, TransactionTrait as _,
};

use super::entity::{client, pairing_code};
use super::secret::{self, Secret};
use super::{Error, Result, Store, check_label};

/// How often a client's `last_used_ms` is written: each request
/// authenticates it, and a write per request would turn reads into
/// writes. A minute is fine enough for `pair ls`.
const LAST_USED_RESOLUTION_MS: i64 = 60_000;

/// A pairing code without its secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingCode {
  pub access: ClientAccess,
  pub id: String,
  /// The name of the client that pairs with it.
  pub name: String,
  pub created_ms: i64,
  pub expires_ms: i64,
  /// The active client that holds the name when the code is created:
  /// pairing with the code replaces it and revokes its token.
  pub replaces: Option<Client>,
}

/// A client without its token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
  pub access: ClientAccess,
  pub id: String,
  pub name: String,
  pub created_ms: i64,
  pub last_used_ms: Option<i64>,
  pub revoked_ms: Option<i64>,
}

/// Whether `token` is shaped like a client token, whether or not a client
/// holds it.
pub fn is_client_token(token: &str) -> bool {
  secret::id(secret::CLIENT, token).is_some()
}

impl Store {
  /// A pairing code for a client named `name`, and its secret, which is
  /// not stored and cannot be shown again. An empty `name` gets a fresh
  /// `client-<code ID>` that no client or pairing code holds, so a code
  /// without a name never replaces a client; the name of an active client
  /// names that client in [`PairingCode::replaces`].
  pub async fn create_pairing_code(&self, name: &str, ttl_ms: i64, now: i64, access: ClientAccess) -> Result<(PairingCode, Secret)> {
    if ttl_ms <= 0 {
      return Err(Error::Invalid("a pairing code needs a positive time to live".into()));
    }
    if !name.is_empty() {
      check_label("client", name)?;
    }
    let txn = self.db.begin().await?;
    let (id, name) = if name.is_empty() {
      fresh_name(&txn).await?
    } else {
      (secret::random_id(), name.to_string())
    };
    check_no_open_code(&txn, &name, now).await?;
    let replaces =
      client::Entity::find().filter(client::Column::Name.eq(&name)).filter(client::Column::RevokedMs.is_null()).one(&txn).await?;
    let replaces = replaces.map(client_of).transpose()?;
    let code = Secret::generate(secret::PAIRING, &id);
    let row = pairing_code::Model {
      access: access_value(access),
      id,
      hash: code.hash(),
      name,
      created_ms: now,
      expires_ms: now.saturating_add(ttl_ms),
      used_ms: None,
    };
    pairing_code::Entity::insert(row.clone().into_active_model()).exec(&txn).await?;
    txn.commit().await?;
    let mut created = pairing_code_of(row)?;
    created.replaces = replaces;
    Ok((created, code))
  }

  /// Spends a pairing code and registers its client, whose token is
  /// returned once, revoking the token of the client that held the name
  /// before, if one did. A pairing that fails spends nothing.
  pub async fn pair(&self, code: &str, now: i64) -> Result<(Client, Secret)> {
    let id = secret::id(secret::PAIRING, code).ok_or(Error::InvalidPairingCode)?;
    let txn = self.db.begin().await?;
    let found = pairing_code::Entity::find_by_id(id).one(&txn).await?;
    let row = found.filter(|row| secret::matches(code, &row.hash)).ok_or(Error::InvalidPairingCode)?;
    if row.used_ms.is_some() {
      return Err(Error::PairingCodeUsed(row.id));
    }
    if now >= row.expires_ms {
      return Err(Error::PairingCodeExpired(row.id));
    }
    // Re-pairing: the client this name names gives up its old token.
    let previous =
      client::Entity::find().filter(client::Column::Name.eq(&row.name)).filter(client::Column::RevokedMs.is_null()).all(&txn).await?;
    for old in previous {
      revoke(&txn, old, now).await?;
    }
    let mut used = row.clone().into_active_model();
    used.used_ms = Set(Some(now));
    used.update(&txn).await?;
    let client_id = secret::random_id();
    let token = Secret::generate(secret::CLIENT, &client_id);
    let access = stored_access(row.access)?;
    let paired = client::Model {
      access: access_value(access),
      id: client_id,
      name: row.name,
      pairing_code: row.id,
      token_hash: Some(token.hash()),
      created_ms: now,
      last_used_ms: None,
      revoked_ms: None,
    };
    client::Entity::insert(paired.clone().into_active_model()).exec(&txn).await?;
    txn.commit().await?;
    Ok((client_of(paired)?, token))
  }

  /// The client `token` belongs to, or `None` when no client holds it.
  /// Records when the client was last seen, to about a minute.
  pub async fn authenticate_client(&self, token: &str, now: i64) -> Result<Option<Client>> {
    let Some(id) = secret::id(secret::CLIENT, token) else {
      return Ok(None);
    };
    let Some(row) = client::Entity::find_by_id(id).one(&self.db).await? else {
      return Ok(None);
    };
    let holds = row.token_hash.as_deref().is_some_and(|stored| secret::matches(token, stored));
    if !holds {
      return Ok(None);
    }
    let mut client = client_of(row.clone())?;
    let stale = row.last_used_ms.is_none_or(|last| now - last >= LAST_USED_RESOLUTION_MS);
    if stale {
      let mut seen = row.clone().into_active_model();
      seen.last_used_ms = Set(Some(now));
      seen.update(&self.db).await?;
    }
    if stale {
      client.last_used_ms = Some(now);
    }
    Ok(Some(client))
  }

  /// Every client, including revoked ones, oldest first.
  pub async fn clients(&self) -> Result<Vec<Client>> {
    let rows = client::Entity::find().order_by_asc(client::Column::CreatedMs).order_by_asc(client::Column::Id).all(&self.db).await?;
    rows.into_iter().map(client_of).collect()
  }

  /// Revokes a client's token. It stays listed as revoked; revoking it
  /// again changes nothing.
  pub async fn revoke_client(&self, id: &str, now: i64) -> Result<()> {
    let row = client::Entity::find_by_id(id).one(&self.db).await?.ok_or_else(|| Error::NoClient(id.to_string()))?;
    if row.revoked_ms.is_none() {
      revoke(&self.db, row, now).await?;
    }
    Ok(())
  }
}

async fn revoke(db: &impl ConnectionTrait, row: client::Model, now: i64) -> Result<()> {
  let mut row = row.into_active_model();
  row.token_hash = Set(None);
  row.revoked_ms = Set(Some(now));
  row.update(db).await?;
  Ok(())
}

/// A code ID and the `client-<code ID>` name it derives, which no client
/// (active or revoked) and no pairing code (open, used or expired) holds.
async fn fresh_name(db: &impl ConnectionTrait) -> Result<(String, String)> {
  // NOTICE: IDs are 48 random bits, so a collision needs an operator to have
  // given a client a name of the same shape; a few tries make the refusal
  // below unreachable in practice.
  for _ in 0..8 {
    let id = secret::random_id();
    let name = format!("client-{id}");
    let client = client::Entity::find().filter(client::Column::Name.eq(&name)).one(db).await?;
    let code = pairing_code::Entity::find()
      .filter(Condition::any().add(pairing_code::Column::Id.eq(&id)).add(pairing_code::Column::Name.eq(&name)))
      .one(db)
      .await?;
    if client.is_none() && code.is_none() {
      return Ok((id, name));
    }
  }
  Err(Error::Invalid("no fresh client name was found; give the pairing code a name".into()))
}

/// Refuses `name` when a pairing code that can still be used will give it,
/// so two codes never race to re-pair one client.
async fn check_no_open_code(db: &impl ConnectionTrait, name: &str, now: i64) -> Result<()> {
  let open = pairing_code::Entity::find()
    .filter(pairing_code::Column::Name.eq(name))
    .filter(pairing_code::Column::UsedMs.is_null())
    .filter(pairing_code::Column::ExpiresMs.gt(now))
    .one(db)
    .await?;
  match open {
    Some(_) => Err(Error::PairingCodeOpen(name.to_string())),
    None => Ok(()),
  }
}

fn pairing_code_of(row: pairing_code::Model) -> Result<PairingCode> {
  Ok(PairingCode {
    access: stored_access(row.access)?,
    id: row.id,
    name: row.name,
    created_ms: row.created_ms,
    expires_ms: row.expires_ms,
    replaces: None,
  })
}

fn client_of(row: client::Model) -> Result<Client> {
  Ok(Client {
    access: stored_access(row.access)?,
    id: row.id,
    name: row.name,
    created_ms: row.created_ms,
    last_used_ms: row.last_used_ms,
    revoked_ms: row.revoked_ms,
  })
}

#[cfg(test)]
#[path = "clients_test.rs"]
mod tests;

fn access_value(access: ClientAccess) -> i32 {
  match access {
    ClientAccess::Runs => 1,
    ClientAccess::Admin => 2,
  }
}

fn stored_access(value: i32) -> Result<ClientAccess> {
  match value {
    0 | 1 => Ok(ClientAccess::Runs),
    2 => Ok(ClientAccess::Admin),
    _ => Err(Error::Invalid("unknown stored client access".into())),
  }
}
