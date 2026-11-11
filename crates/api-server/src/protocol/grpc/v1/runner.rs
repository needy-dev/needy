//! `RunnerService` over [`Runners`].
//!
//! The middleware identifies the caller of each request: an operator holds
//! the control token, a runner its credential. `Join` is public; the join
//! token in the request authenticates it.

use std::sync::Arc;

use needy_api_proto::v1::runner_service_server::RunnerService;
use needy_api_proto::v1::{ControlRequest, ControlResponse, TaskRequest, TaskResponse, task_request};
use needy_api_proto::v1::{
  CreateJoinTokenRequest, CreateJoinTokenResponse, HeartbeatRequest, HeartbeatResponse, JoinRequest, JoinResponse, ListJoinTokensRequest,
  ListJoinTokensResponse, ListRunnersRequest, ListRunnersResponse, RemoveRunnerRequest, RemoveRunnerResponse, RevokeJoinTokenRequest,
  RevokeJoinTokenResponse, RotateRunnerRequest, RotateRunnerResponse,
};
use tokio_stream::StreamExt as _;
use tonic::{Request, Response, Status, Streaming};

use super::{Streamed, administer, administer_locally, caller};
use crate::control::{Caller, ControlError, Runners};
use crate::protocol::domain;

pub struct Adapter<R> {
  pub runners: Arc<R>,
  pub caller: Caller,
}

impl<R> Adapter<R> {
  /// Refuses a call only an operator or a client may make.
  fn operator<T>(&self, request: &Request<T>) -> Result<(), Status> {
    match caller(&self.caller, request) {
      Caller::Operator | Caller::Client { .. } => Ok(()),
      Caller::Actor { .. } | Caller::Runner { .. } => Err(ControlError::denied("only operators manage runners").into()),
    }
  }

  /// Refuses a call only the operator may make: a client is refused too.
  fn admin<T>(&self, request: &Request<T>, what: &str) -> Result<(), Status> {
    administer(&self.caller, request, what, "only operators manage runners")
  }

  /// The runner making a call only a runner may make.
  fn runner<T>(&self, request: &Request<T>, what: &str) -> Result<String, Status> {
    match caller(&self.caller, request) {
      Caller::Runner { name } => Ok(name),
      Caller::Operator | Caller::Client { .. } | Caller::Actor { .. } => Err(ControlError::denied(format!("only runners {what}")).into()),
    }
  }
}

#[tonic::async_trait]
impl<R: Runners> RunnerService for Adapter<R> {
  type ControlStream = Streamed<ControlResponse>;
  type TaskStream = Streamed<TaskResponse>;

  async fn control(&self, request: Request<Streaming<ControlRequest>>) -> Result<Response<Self::ControlStream>, Status> {
    let runner = self.runner(&request, "open a control stream")?;
    let tasks = self.runners.control(&runner, Box::pin(request.into_inner())).await?;
    Ok(Response::new(Box::pin(tasks.map(Ok))))
  }

  async fn task(&self, request: Request<Streaming<TaskRequest>>) -> Result<Response<Self::TaskStream>, Status> {
    let runner = self.runner(&request, "open task streams")?;
    let mut inbound = request.into_inner();
    let task = match inbound.message().await? {
      Some(TaskRequest {
        payload: Some(task_request::Payload::TaskId(task)),
      }) => task,
      _ => return Err(ControlError::Invalid("the first request of a task stream must name its task".into()).into()),
    };
    let requests = self.runners.task(&runner, task, Box::pin(inbound)).await?;
    Ok(Response::new(Box::pin(requests.map(Ok))))
  }

  async fn create_join_token(&self, request: Request<CreateJoinTokenRequest>) -> Result<Response<CreateJoinTokenResponse>, Status> {
    self.admin(&request, "creating join tokens")?;
    let request = domain::new_join_token(request.into_inner()).map_err(ControlError::from)?;
    let created = self.runners.create_join_token(request).await?;
    Ok(Response::new(CreateJoinTokenResponse {
      join_token: Some(domain::proto_join_token(created.info)),
      token: created.token,
    }))
  }

  async fn list_join_tokens(&self, request: Request<ListJoinTokensRequest>) -> Result<Response<ListJoinTokensResponse>, Status> {
    administer_locally(&self.caller, &request, "listing join tokens", "only operators manage runners")?;
    let join_tokens = self.runners.join_tokens().await?.into_iter().map(domain::proto_join_token).collect();
    Ok(Response::new(ListJoinTokensResponse { join_tokens }))
  }

  async fn revoke_join_token(&self, request: Request<RevokeJoinTokenRequest>) -> Result<Response<RevokeJoinTokenResponse>, Status> {
    self.admin(&request, "revoking join tokens")?;
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
    self.admin(&request, "rotating runners")?;
    self.runners.rotate(&request.into_inner().name).await?;
    Ok(Response::new(RotateRunnerResponse {}))
  }

  async fn remove_runner(&self, request: Request<RemoveRunnerRequest>) -> Result<Response<RemoveRunnerResponse>, Status> {
    self.admin(&request, "removing runners")?;
    self.runners.remove(&request.into_inner().name).await?;
    Ok(Response::new(RemoveRunnerResponse {}))
  }
}

#[cfg(test)]
#[path = "runner_test.rs"]
mod tests;
