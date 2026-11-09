//! `SessionService` over [`Sessions`].

use std::sync::Arc;

use needy_api_proto::v1::session_service_server::SessionService;
use needy_api_proto::v1::{
  CreateSessionRequest, CreateSessionResponse, DecideRequest, DecideResponse, GetSessionLogsRequest, GetSessionLogsResponse,
  GetSessionRequest, GetSessionResponse, ListSessionsRequest, ListSessionsResponse, StopSessionRequest, StopSessionResponse,
};
use tonic::{Request, Response, Status};

use super::{Streamed, chunked};
use crate::control::{Caller, ControlError, Sessions};
use crate::protocol::domain;

pub struct Adapter<S> {
  pub sessions: Arc<S>,
  pub caller: Caller,
}

impl<S> Adapter<S> {
  /// Refuses a call only the control socket may make.
  fn cli_only(&self, refusal: &str) -> Result<(), Status> {
    match self.caller {
      Caller::Cli => Ok(()),
      Caller::Actor { .. } => Err(ControlError::denied(refusal).into()),
    }
  }
}

#[tonic::async_trait]
impl<S: Sessions> SessionService for Adapter<S> {
  async fn create_session(&self, request: Request<CreateSessionRequest>) -> Result<Response<CreateSessionResponse>, Status> {
    self.cli_only("actors cannot create sessions")?;
    let request = domain::new_session(request.into_inner()).map_err(ControlError::from)?;
    let session = self.sessions.create(request).await?;
    Ok(Response::new(CreateSessionResponse {
      session: Some(domain::proto_session(session)),
    }))
  }

  async fn get_session(&self, request: Request<GetSessionRequest>) -> Result<Response<GetSessionResponse>, Status> {
    let session = self.sessions.get(&request.into_inner().session_id).await?;
    Ok(Response::new(GetSessionResponse {
      session: Some(domain::proto_session(session)),
    }))
  }

  async fn list_sessions(&self, _: Request<ListSessionsRequest>) -> Result<Response<ListSessionsResponse>, Status> {
    let sessions = self.sessions.list().await?.into_iter().map(domain::proto_session).collect();
    Ok(Response::new(ListSessionsResponse { sessions }))
  }

  async fn stop_session(&self, request: Request<StopSessionRequest>) -> Result<Response<StopSessionResponse>, Status> {
    self.cli_only("actors cannot stop sessions")?;
    self.sessions.stop(&request.into_inner().session_id).await?;
    Ok(Response::new(StopSessionResponse {}))
  }

  type GetSessionLogsStream = Streamed<GetSessionLogsResponse>;

  async fn get_session_logs(&self, request: Request<GetSessionLogsRequest>) -> Result<Response<Self::GetSessionLogsStream>, Status> {
    self.cli_only("actors cannot read session logs")?;
    let console = self.sessions.logs(&request.into_inner().session_id).await?;
    Ok(Response::new(chunked(console, |data| GetSessionLogsResponse { data })))
  }

  async fn decide(&self, request: Request<DecideRequest>) -> Result<Response<DecideResponse>, Status> {
    let mut request = request.into_inner();
    // An actor decides within its own session, whatever it names.
    let session = match &self.caller {
      Caller::Actor { session } => session.clone(),
      Caller::Cli => std::mem::take(&mut request.session_id),
    };
    let route = self.sessions.decide(&session, domain::exec_context(request)).await?;
    Ok(Response::new(domain::proto_route(route)))
  }
}

#[cfg(test)]
#[path = "session_test.rs"]
mod tests;
