//! Domain types and trait for `ClientService`.

use super::{Credential, Result};

/// The authority granted by the server, independent of transport.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ClientAccess {
  /// Legacy omitted grants can drive runs and processes.
  #[default]
  Runs,
  /// May also manage registered clients, runners and invitations.
  Admin,
}

/// A pairing code to create.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewPairingCode {
  pub access: ClientAccess,
  pub ttl_ms: i64,
  /// The client's name once it pairs; empty for a fresh one derived from
  /// the code's ID, which no client holds.
  pub name: String,
}

/// A pairing code just created, with its secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedPairingCode {
  pub access: ClientAccess,
  pub id: String,
  pub name: String,
  pub expires_ms: i64,
  pub code: String,
  /// The active client that holds `name` now: pairing with the code
  /// replaces it and revokes its token.
  pub replaces: Option<ClientInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInfo {
  pub access: ClientAccess,
  pub id: String,
  pub name: String,
  pub created_ms: i64,
  pub last_used_ms: Option<i64>,
  pub revoked_ms: Option<i64>,
}

/// A client that paired, with its token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paired {
  pub client: ClientInfo,
  pub token: String,
}

/// The control plane's client registry: pairing codes and the clients
/// that paired with them.
// TODO(clients): listing and revoking pairing codes is left out: a code is
// single-use and short-lived (10 minutes by default), so an unused one
// expires before it matters. Add `ListPairingCodes` and a revoke when an
// owner asks for longer-lived codes.
#[async_trait::async_trait]
pub trait Clients: Send + Sync + 'static {
  async fn create_pairing_code(&self, request: NewPairingCode) -> Result<CreatedPairingCode>;
  /// Spends a pairing code and returns the new client's token.
  async fn pair(&self, code: &str) -> Result<Paired>;
  /// Every client, including revoked ones.
  async fn clients(&self) -> Result<Vec<ClientInfo>>;
  async fn revoke(&self, id: &str) -> Result<()>;
  /// Whose token a bearer token is. The middleware calls it for every
  /// token that is not the control token. Backend failures are errors;
  /// invalid credentials are `Credential::Refused`.
  async fn credential(&self, token: &str) -> Result<Credential>;
}
