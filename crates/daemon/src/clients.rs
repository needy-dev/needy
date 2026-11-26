//! The control plane's client registry (`ClientService`) over `state.db`:
//! pairing codes, pairing, listing and revoking clients, and checking a
//! client's token for the middleware. Static clients from
//! `--clients-file` ([`crate::static_clients`]) are listed and authenticated
//! next to paired ones.

use needy_api_server::control::{ClientInfo, Clients, ControlError, CreatedPairingCode, Credential, NewPairingCode, Paired, Result};
use needy_common::time::now_ms;

use crate::Daemon;
use crate::runners::control_error;
use crate::static_clients::{Check, StaticClient};
use crate::store;

fn client_info(client: store::Client) -> ClientInfo {
  ClientInfo {
    access: client.access,
    id: client.id,
    name: client.name,
    created_ms: client.created_ms,
    last_used_ms: client.last_used_ms,
    revoked_ms: client.revoked_ms,
  }
}

// TODO(clients-file): `ClientInfo` and the proto `Client` have no static
// marker like `Runner.static_config`; a static client shows as its
// `static-NAME` ID instead. Adding a field is a proto change outside the
// `--clients-file` slice; reopen when `client ls` needs to show it apart.
fn static_client_info(client: StaticClient) -> ClientInfo {
  ClientInfo {
    access: client.access,
    id: client.id,
    name: client.name,
    created_ms: client.created_ms,
    last_used_ms: client.last_used_ms,
    revoked_ms: None,
  }
}

impl Daemon {
  // NOTICE: a token that both files hold is refused here and in
  // `unprefixed_runner` instead of when either file loads. The files reload
  // independently, so a load-time check against the other file's last
  // loaded entries could refuse both files for good when an orchestrator
  // moves one secret from one file to the other. Each file refuses its own
  // duplicate hashes when it loads.
  /// What an unprefixed bearer token is to the client registry: only a
  /// static client's token can be one (paired clients' tokens are always
  /// prefixed), found by its hash.
  fn unprefixed_client(&self, token: &str) -> Credential {
    self.static_clients.reload();
    let Some(name) = self.static_clients.holds(token) else {
      return Credential::Other;
    };
    self.static_runners.reload(&self.links);
    if let Some(runner) = self.static_runners.find(token) {
      tracing::warn!("static client {name} and static runner {runner} have the same token; refusing it for both");
      return Credential::Refused;
    }
    match self.static_clients.find(token, now_ms()) {
      Some(client) => Credential::Client {
        id: client.id,
        name: client.name,
        access: client.access,
      },
      None => Credential::Other,
    }
  }
}

#[async_trait::async_trait]
impl Clients for Daemon {
  async fn create_pairing_code(&self, request: NewPairingCode) -> Result<CreatedPairingCode> {
    self.static_clients.reload();
    if self.static_clients.holds_name(&request.name) {
      return Err(ControlError::Failed(format!(
        "client {} is statically configured in the clients file and cannot be paired",
        request.name
      )));
    }
    let created = self.state.create_pairing_code(&request.name, request.ttl_ms, now_ms(), request.access).await;
    let (code, secret) = created.map_err(|error| control_error(error, true))?;
    match &code.replaces {
      Some(old) => tracing::info!("pairing code {} created for client {}, replacing client {}", code.id, code.name, old.id),
      None => tracing::info!("pairing code {} created for client {}", code.id, code.name),
    }
    Ok(CreatedPairingCode {
      access: code.access,
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
      control_error(e, true)
    })?;
    tracing::info!("client {} ({}) paired", client.name, client.id);
    Ok(Paired {
      client: client_info(client),
      token: token.into_string(),
    })
  }

  async fn clients(&self) -> Result<Vec<ClientInfo>> {
    self.static_clients.reload();
    let clients = self.state.clients().await.map_err(|error| control_error(error, false))?;
    let paired = clients.into_iter().map(client_info);
    Ok(paired.chain(self.static_clients.list().into_iter().map(static_client_info)).collect())
  }

  async fn revoke(&self, id: &str) -> Result<()> {
    self.static_clients.reload();
    if self.static_clients.contains(id) {
      return Err(ControlError::Failed(format!("client {id} is statically configured; remove it from the clients file instead")));
    }
    self.state.revoke_client(id, now_ms()).await.map_err(|error| control_error(error, true))?;
    tracing::info!("client {id} revoked");
    Ok(())
  }

  async fn credential(&self, token: &str) -> Result<Credential> {
    if store::is_runner_credential(token) {
      return Ok(Credential::Other);
    }
    if !store::is_client_token(token) {
      return Ok(self.unprefixed_client(token));
    }
    let now = now_ms();
    self.static_clients.reload();
    match self.static_clients.authenticate(token, now) {
      Check::Client(client) => {
        return Ok(Credential::Client {
          id: client.id,
          name: client.name,
          access: client.access,
        });
      }
      Check::Refused => return Ok(Credential::Refused),
      Check::NotStatic => {}
    }
    Ok(match self.state.authenticate_client(token, now).await {
      // The clients file is the authority for a name it holds: a paired
      // client of the same name is refused while the entry exists.
      Ok(Some(client)) if self.static_clients.holds_name(&client.name) => {
        tracing::warn!("client {} ({}) is refused: a static client holds its name", client.name, client.id);
        Credential::Refused
      }
      Ok(Some(client)) => Credential::Client {
        id: client.id,
        name: client.name,
        access: client.access,
      },
      Ok(None) => Credential::Refused,
      Err(e) => {
        tracing::error!("state.db: cannot check a client token: {e:#}");
        return Err(ControlError::Unavailable("authentication registry unavailable".into()).with_diagnostic("auth.backend_unavailable", []));
      }
    })
  }
}

#[cfg(test)]
#[path = "clients_test.rs"]
mod tests;
