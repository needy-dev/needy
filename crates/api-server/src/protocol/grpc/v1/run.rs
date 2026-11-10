//! `RunService` over [`Runs`].

use std::sync::Arc;

use needy_api_proto::v1::run_service_server::RunService;
use needy_api_proto::v1::{
  CheckProfileRequest, CheckProfileResponse, CreateRunRequest, CreateRunResponse, DecideRequest, DecideResponse, GetRunLogsRequest,
  GetRunLogsResponse, GetRunRequest, GetRunResponse, ListRunsRequest, ListRunsResponse, StopRunRequest, StopRunResponse,
};
use tonic::{Request, Response, Status};

use super::{Streamed, chunked};
use crate::control::{Caller, ControlError, Runs};
use crate::protocol::domain;

pub struct Adapter<S> {
  pub runs: Arc<S>,
  pub caller: Caller,
}

impl<S> Adapter<S> {
  /// Refuses a call only the control socket may make.
  fn cli_only(&self, refusal: &str) -> Result<(), Status> {
    match self.caller {
      Caller::Cli => Ok(()),
      Caller::Actor { .. } => Err(ControlError::denied(refusal).into()),
      Caller::Runner { .. } => Err(ControlError::denied("runners cannot manage runs").into()),
    }
  }
}

#[tonic::async_trait]
impl<S: Runs> RunService for Adapter<S> {
  async fn create_run(&self, request: Request<CreateRunRequest>) -> Result<Response<CreateRunResponse>, Status> {
    self.cli_only("actors cannot create runs")?;
    let request = domain::new_run(request.into_inner()).map_err(ControlError::from)?;
    let run = self.runs.create(request).await?;
    Ok(Response::new(CreateRunResponse {
      run: Some(domain::proto_run(run)),
    }))
  }

  async fn get_run(&self, request: Request<GetRunRequest>) -> Result<Response<GetRunResponse>, Status> {
    let run = self.runs.get(&request.into_inner().run_id).await?;
    Ok(Response::new(GetRunResponse {
      run: Some(domain::proto_run(run)),
    }))
  }

  async fn list_runs(&self, _: Request<ListRunsRequest>) -> Result<Response<ListRunsResponse>, Status> {
    let runs = self.runs.list().await?.into_iter().map(domain::proto_run).collect();
    Ok(Response::new(ListRunsResponse { runs }))
  }

  async fn stop_run(&self, request: Request<StopRunRequest>) -> Result<Response<StopRunResponse>, Status> {
    self.cli_only("actors cannot stop runs")?;
    self.runs.stop(&request.into_inner().run_id).await?;
    Ok(Response::new(StopRunResponse {}))
  }

  type GetRunLogsStream = Streamed<GetRunLogsResponse>;

  async fn get_run_logs(&self, request: Request<GetRunLogsRequest>) -> Result<Response<Self::GetRunLogsStream>, Status> {
    self.cli_only("actors cannot read run logs")?;
    let console = self.runs.logs(&request.into_inner().run_id).await?;
    Ok(Response::new(chunked(console, |data| GetRunLogsResponse { data })))
  }

  async fn decide(&self, request: Request<DecideRequest>) -> Result<Response<DecideResponse>, Status> {
    let mut request = request.into_inner();
    // An actor decides within its own run, whatever it names.
    let run = match &self.caller {
      Caller::Actor { run } => run.clone(),
      Caller::Cli => std::mem::take(&mut request.run_id),
      Caller::Runner { .. } => return Err(ControlError::denied("runners cannot place commands").into()),
    };
    let route = self.runs.decide(&run, domain::exec_context(request)).await?;
    Ok(Response::new(domain::proto_route(route)))
  }

  async fn check_profile(&self, request: Request<CheckProfileRequest>) -> Result<Response<CheckProfileResponse>, Status> {
    self.cli_only("actors cannot check profiles")?;
    let check = self.runs.check(domain::profile_to_check(request.into_inner())).await?;
    Ok(Response::new(domain::proto_profile_check(check)))
  }
}

#[cfg(test)]
#[path = "run_test.rs"]
mod tests;
