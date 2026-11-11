//! The control plane's client registry (`ClientService`) over `state.db`:
//! pairing codes, pairing, listing and revoking clients, and checking a
//! client's token for the middleware.

use needy_api_server::control::{ClientInfo, Clients, CreatedPairingCode, Credential, NewPairingCode, Paired, Result};
use needy_common::time::now_ms;

use crate::Daemon;
use crate::runners::control_error;
use crate::store;

fn client_info(client: store::Client) -> ClientInfo {
  ClientInfo {
    id: client.id,
    name: client.name,
    created_ms: client.created_ms,
    last_used_ms: client.last_used_ms,
    revoked_ms: client.revoked_ms,
  }
}

#[async_trait::async_trait]
impl Clients for Daemon {
  async fn create_pairing_code(&self, request: NewPairingCode) -> Result<CreatedPairingCode> {
    let created = self.state.create_pairing_code(&request.name, request.ttl_ms, now_ms()).await;
    let (code, secret) = created.map_err(control_error)?;
    match &code.replaces {
      Some(old) => tracing::info!("pairing code {} created for client {}, replacing client {}", code.id, code.name, old.id),
      None => tracing::info!("pairing code {} created for client {}", code.id, code.name),
    }
    Ok(CreatedPairingCode {
      id: code.id,
      name: code.name,
      expires_ms: code.expires_ms,
      code: secret.into_string(),
      replaces: code.replaces.map(client_info),
    })
  }

  async fn pair(&self, code: &str) -> Result<Paired> {
    let (client, token) = self.state.pair(code, now_ms()).await.map_err(|e| {
      tracing::warn!("a client cannot pair: {e}");
      control_error(e)
    })?;
    tracing::info!("client {} ({}) paired", client.name, client.id);
    Ok(Paired {
      client: client_info(client),
      token: token.into_string(),
    })
  }

  async fn clients(&self) -> Result<Vec<ClientInfo>> {
    let clients = self.state.clients().await.map_err(control_error)?;
    Ok(clients.into_iter().map(client_info).collect())
  }

  async fn revoke(&self, id: &str) -> Result<()> {
    self.state.revoke_client(id, now_ms()).await.map_err(control_error)?;
    tracing::info!("client {id} revoked");
    Ok(())
  }

  async fn credential(&self, token: &str) -> Credential {
    if !store::is_client_token(token) {
      return Credential::Other;
    }
    match self.state.authenticate_client(token, now_ms()).await {
      Ok(Some(client)) => Credential::Client {
        id: client.id,
        name: client.name,
      },
      Ok(None) => Credential::Refused,
      Err(e) => {
        tracing::error!("state.db: cannot check a client token: {e:#}");
        Credential::Refused
      }
    }
  }
}

#[cfg(test)]
#[path = "clients_test.rs"]
mod tests;
