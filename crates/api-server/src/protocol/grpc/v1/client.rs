//! `ClientService` over [`Clients`].
//!
//! An operator (the control token or the control socket) and every paired
//! client may list clients; only the operator creates pairing codes and
//! revokes clients. `Pair` is public; the pairing code in the request
//! authenticates it.

use std::sync::Arc;

use needy_api_proto::v1::client_service_server::ClientService;
use needy_api_proto::v1::{
  CreatePairingCodeRequest, CreatePairingCodeResponse, ListClientsRequest, ListClientsResponse, PairRequest, PairResponse,
  RevokeClientRequest, RevokeClientResponse,
};
use tonic::{Request, Response, Status};

use super::{administer, caller};
use crate::control::{Caller, Clients, ControlError, NewPairingCode};
use crate::protocol::domain;

pub struct Adapter<C> {
  pub clients: Arc<C>,
  pub caller: Caller,
}

impl<C> Adapter<C> {
  /// Refuses a call only an operator or a client may make.
  fn operator<T>(&self, request: &Request<T>) -> Result<(), Status> {
    if caller(&self.caller, request).is_operator() {
      return Ok(());
    }
    Err(ControlError::denied("only operators and clients manage clients").into())
  }

  /// Refuses a call only the operator may make: a client is refused too.
  fn admin<T>(&self, request: &Request<T>, what: &str) -> Result<(), Status> {
    administer(&self.caller, request, what, "only operators manage clients")
  }
}

#[tonic::async_trait]
impl<C: Clients> ClientService for Adapter<C> {
  async fn create_pairing_code(&self, request: Request<CreatePairingCodeRequest>) -> Result<Response<CreatePairingCodeResponse>, Status> {
    self.admin(&request, "creating pairing codes")?;
    let request = request.into_inner();
    let created = self
      .clients
      .create_pairing_code(NewPairingCode {
        ttl_ms: request.ttl_ms,
        name: request.name,
      })
      .await?;
    Ok(Response::new(CreatePairingCodeResponse {
      id: created.id,
      name: created.name,
      expires_at_ms: created.expires_ms,
      code: created.code,
    }))
  }

  async fn pair(&self, request: Request<PairRequest>) -> Result<Response<PairResponse>, Status> {
    let code = request.into_inner().code;
    if code.is_empty() {
      return Err(ControlError::Invalid("the request names no pairing code".into()).into());
    }
    let paired = self.clients.pair(&code).await?;
    Ok(Response::new(PairResponse {
      client: Some(domain::proto_client(paired.client)),
      token: paired.token,
    }))
  }

  async fn list_clients(&self, request: Request<ListClientsRequest>) -> Result<Response<ListClientsResponse>, Status> {
    self.operator(&request)?;
    let clients = self.clients.clients().await?.into_iter().map(domain::proto_client).collect();
    Ok(Response::new(ListClientsResponse { clients }))
  }

  async fn revoke_client(&self, request: Request<RevokeClientRequest>) -> Result<Response<RevokeClientResponse>, Status> {
    self.admin(&request, "revoking clients")?;
    self.clients.revoke(&request.into_inner().id).await?;
    Ok(Response::new(RevokeClientResponse {}))
  }
}

#[cfg(test)]
#[path = "client_test.rs"]
mod tests;
