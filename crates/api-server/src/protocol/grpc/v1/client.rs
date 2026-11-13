//! `ClientService` over [`Clients`].
//!
//! Local operators and administrator clients manage registrations over any
//! admitted listener. `Pair` authenticates with its single-use invitation.

use std::sync::Arc;

use needy_api_proto::v1::client_service_server::ClientService;
use needy_api_proto::v1::{
  CallerKind, CreatePairingCodeRequest, CreatePairingCodeResponse, GetCallerRequest, GetCallerResponse, ListClientsRequest,
  ListClientsResponse, PairRequest, PairResponse, RevokeClientRequest, RevokeClientResponse,
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
  /// Refuses management calls without operator or administrator authority.
  fn admin<T>(&self, request: &Request<T>, what: &str) -> Result<(), Status> {
    administer(&self.caller, request, what, "only operators manage clients")
  }
}

#[tonic::async_trait]
impl<C: Clients> ClientService for Adapter<C> {
  async fn get_caller(&self, request: Request<GetCallerRequest>) -> Result<Response<GetCallerResponse>, Status> {
    let response = match caller(&self.caller, &request) {
      Caller::Operator => GetCallerResponse {
        kind: CallerKind::LocalOperator as i32,
        access: needy_api_proto::v1::ClientAccess::Admin as i32,
        ..Default::default()
      },
      Caller::Client { id, name, access } => GetCallerResponse {
        kind: CallerKind::Client as i32,
        client_id: id,
        name,
        access: domain::proto_client_access(access),
      },
      _ => return Err(ControlError::role_denied("caller inspection is available to operators and clients").into()),
    };
    Ok(Response::new(response))
  }

  async fn create_pairing_code(&self, request: Request<CreatePairingCodeRequest>) -> Result<Response<CreatePairingCodeResponse>, Status> {
    self.admin(&request, "creating pairing codes")?;
    let request = request.into_inner();
    let created = self
      .clients
      .create_pairing_code(NewPairingCode {
        access: domain::client_access(request.access).map_err(ControlError::from)?,
        ttl_ms: request.ttl_ms,
        name: request.name,
      })
      .await?;
    Ok(Response::new(CreatePairingCodeResponse {
      access: domain::proto_client_access(created.access),
      id: created.id,
      name: created.name,
      expires_at_ms: created.expires_ms,
      code: created.code,
      replaces: created.replaces.map(domain::proto_client),
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
    self.admin(&request, "listing clients")?;
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
