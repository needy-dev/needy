//! `RunnerService` over [`Runners`].
//!
//! The middleware identifies the caller of each request: an operator holds
//! the control token, a runner its credential. `Join` is public; the join
//! token in the request authenticates it.

use std::sync::Arc;

use needy_api_proto::v1::runner_service_server::RunnerService;
use needy_api_proto::v1::{
  CreateJoinTokenRequest, CreateJoinTokenResponse, HeartbeatRequest, HeartbeatResponse, JoinRequest, JoinResponse, ListJoinTokensRequest,
  ListJoinTokensResponse, ListRunnersRequest, ListRunnersResponse, RemoveRunnerRequest, RemoveRunnerResponse, RevokeJoinTokenRequest,
  RevokeJoinTokenResponse, RotateRunnerRequest, RotateRunnerResponse,
};
use tonic::{Request, Response, Status};

use super::caller;
use crate::control::{Caller, ControlError, Runners};
use crate::protocol::domain;

pub struct Adapter<R> {
  pub runners: Arc<R>,
  pub caller: Caller,
}

impl<R> Adapter<R> {
  /// Refuses a call only an operator may make.
  fn operator<T>(&self, request: &Request<T>) -> Result<(), Status> {
    match caller(&self.caller, request) {
      Caller::Cli => Ok(()),
      Caller::Actor { .. } | Caller::Runner { .. } => Err(ControlError::denied("only operators manage runners").into()),
    }
  }
}

#[tonic::async_trait]
impl<R: Runners> RunnerService for Adapter<R> {
  async fn create_join_token(&self, request: Request<CreateJoinTokenRequest>) -> Result<Response<CreateJoinTokenResponse>, Status> {
    self.operator(&request)?;
    let request = domain::new_join_token(request.into_inner()).map_err(ControlError::from)?;
    let created = self.runners.create_join_token(request).await?;
    Ok(Response::new(CreateJoinTokenResponse {
      join_token: Some(domain::proto_join_token(created.info)),
      token: created.token,
    }))
  }

  async fn list_join_tokens(&self, request: Request<ListJoinTokensRequest>) -> Result<Response<ListJoinTokensResponse>, Status> {
    self.operator(&request)?;
    let join_tokens = self.runners.join_tokens().await?.into_iter().map(domain::proto_join_token).collect();
    Ok(Response::new(ListJoinTokensResponse { join_tokens }))
  }

  async fn revoke_join_token(&self, request: Request<RevokeJoinTokenRequest>) -> Result<Response<RevokeJoinTokenResponse>, Status> {
    self.operator(&request)?;
    self.runners.revoke_join_token(&request.into_inner().id).await?;
    Ok(Response::new(RevokeJoinTokenResponse {}))
  }

  async fn join(&self, request: Request<JoinRequest>) -> Result<Response<JoinResponse>, Status> {
    let request = domain::joining(request.into_inner()).map_err(ControlError::from)?;
    let joined = self.runners.join(request).await?;
    Ok(Response::new(JoinResponse {
      runner: Some(domain::proto_runner(joined.runner)),
      credential: joined.credential,
      heartbeat_interval_ms: joined.heartbeat_interval_ms,
    }))
  }

  async fn heartbeat(&self, request: Request<HeartbeatRequest>) -> Result<Response<HeartbeatResponse>, Status> {
    let Caller::Runner { name } = caller(&self.caller, &request) else {
      return Err(ControlError::denied("only runners send heartbeats").into());
    };
    let report = domain::runner_report(request.into_inner()).map_err(ControlError::from)?;
    let beat = self.runners.heartbeat(&name, report).await?;
    Ok(Response::new(HeartbeatResponse {
      credential: beat.credential.unwrap_or_default(),
      heartbeat_interval_ms: beat.heartbeat_interval_ms,
    }))
  }

  async fn list_runners(&self, request: Request<ListRunnersRequest>) -> Result<Response<ListRunnersResponse>, Status> {
    self.operator(&request)?;
    let runners = self.runners.runners().await?.into_iter().map(domain::proto_runner).collect();
    Ok(Response::new(ListRunnersResponse { runners }))
  }

  async fn rotate_runner(&self, request: Request<RotateRunnerRequest>) -> Result<Response<RotateRunnerResponse>, Status> {
    self.operator(&request)?;
    self.runners.rotate(&request.into_inner().name).await?;
    Ok(Response::new(RotateRunnerResponse {}))
  }

  async fn remove_runner(&self, request: Request<RemoveRunnerRequest>) -> Result<Response<RemoveRunnerResponse>, Status> {
    self.operator(&request)?;
    self.runners.remove(&request.into_inner().name).await?;
    Ok(Response::new(RemoveRunnerResponse {}))
  }
}

#[cfg(test)]
#[path = "runner_test.rs"]
mod tests;
