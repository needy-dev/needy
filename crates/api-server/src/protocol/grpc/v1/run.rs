//! `RunService` over [`Runs`].

use std::sync::Arc;

use futures_util::StreamExt as _;
use needy_api_proto::v1::run_service_server::RunService;
use needy_api_proto::v1::{
  AddFaultRequest, AddFaultResponse, CheckProfileRequest, CheckProfileResponse, CreateRunRequest, CreateRunResponse, DecideRequest,
  DecideResponse, GetRunEventsRequest, GetRunEventsResponse, GetRunLogsRequest, GetRunLogsResponse, GetRunRequest, GetRunResponse,
  ListFaultsRequest, ListFaultsResponse, ListRunsRequest, ListRunsResponse, RemoveFaultRequest, RemoveFaultResponse, StopRunRequest,
  StopRunResponse,
};
use tonic::{Request, Response, Status};

use super::{CHUNK, Streamed, caller};
use crate::control::{Caller, ControlError, Runs};
use crate::protocol::{domain, status};

// NOTICE: a stored event is a few hundred bytes; 1000 of them stay well
// under gRPC's default 4 MiB message limit.
const EVENTS_PER_MESSAGE: usize = 1000;

pub struct Adapter<S> {
  pub runs: Arc<S>,
  pub caller: Caller,
}

impl<S> Adapter<S> {
  /// The caller of a call only an operator or a client may make.
  fn operator<T>(&self, request: &Request<T>, refusal: &str) -> Result<Caller, Status> {
    match caller(&self.caller, request) {
      caller @ (Caller::Operator | Caller::Client { .. }) => Ok(caller),
      Caller::Actor { .. } => Err(ControlError::denied(refusal).into()),
      Caller::Runner { .. } => Err(ControlError::denied("runners cannot manage runs").into()),
    }
  }
}

#[tonic::async_trait]
impl<S: Runs> RunService for Adapter<S> {
  async fn create_run(&self, request: Request<CreateRunRequest>) -> Result<Response<CreateRunResponse>, Status> {
    let caller = self.operator(&request, "actors cannot create runs")?;
    let mut request = domain::new_run(request.into_inner()).map_err(ControlError::from)?;
    request.by = Some(caller.principal());
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
    let caller = self.operator(&request, "actors cannot stop runs")?;
    let request = request.into_inner();
    let mode = domain::stop_mode(request.now, request.grace_ms).map_err(ControlError::from)?;
    let stopped = self.runs.stop(&request.run_id, caller.principal(), mode).await?;
    Ok(Response::new(domain::proto_stopped(stopped)))
  }

  type GetRunLogsStream = Streamed<GetRunLogsResponse>;

  async fn get_run_logs(&self, request: Request<GetRunLogsRequest>) -> Result<Response<Self::GetRunLogsStream>, Status> {
    self.operator(&request, "actors cannot read run logs")?;
    let request = request.into_inner();
    let console = self.runs.logs(&request.run_id, request.follow).await?;
    // Chunks stay under gRPC's default 4 MiB message limit.
    let messages = console.flat_map(|read| {
      let messages: Vec<Result<GetRunLogsResponse, Status>> = match read {
        Ok(bytes) => bytes
          .chunks(CHUNK)
          .map(|data| {
            Ok(GetRunLogsResponse {
              data: data.to_vec(),
            })
          })
          .collect(),
        Err(e) => vec![Err(status::status(e))],
      };
      tokio_stream::iter(messages)
    });
    Ok(Response::new(Box::pin(messages)))
  }

  type GetRunEventsStream = Streamed<GetRunEventsResponse>;

  async fn get_run_events(&self, request: Request<GetRunEventsRequest>) -> Result<Response<Self::GetRunEventsStream>, Status> {
    self.operator(&request, "actors cannot read run events")?;
    let loaded = self.runs.events(&request.into_inner().run_id).await?;
    let run = loaded.run;
    let mut events = loaded.events.into_iter().map(domain::proto_stored_event).collect::<Vec<_>>();
    let mut messages = Vec::new();
    // A run without events is still one message, which names the run.
    loop {
      let rest = events.split_off(events.len().min(EVENTS_PER_MESSAGE));
      messages.push(Ok(GetRunEventsResponse {
        run_id: run.clone(),
        events,
      }));
      if rest.is_empty() {
        break;
      }
      events = rest;
    }
    Ok(Response::new(Box::pin(tokio_stream::iter(messages))))
  }

  async fn decide(&self, request: Request<DecideRequest>) -> Result<Response<DecideResponse>, Status> {
    let caller = caller(&self.caller, &request);
    let mut request = request.into_inner();
    // An actor decides within its own run, whatever it names.
    let run = match &caller {
      Caller::Actor { run } => run.clone(),
      Caller::Operator | Caller::Client { .. } => std::mem::take(&mut request.run_id),
      Caller::Runner { .. } => return Err(ControlError::denied("runners cannot place commands").into()),
    };
    let route = self.runs.decide(&run, domain::exec_context(request)).await?;
    Ok(Response::new(domain::proto_route(route)))
  }

  async fn check_profile(&self, request: Request<CheckProfileRequest>) -> Result<Response<CheckProfileResponse>, Status> {
    self.operator(&request, "actors cannot check profiles")?;
    let check = self.runs.check(domain::profile_to_check(request.into_inner())).await?;
    Ok(Response::new(domain::proto_profile_check(check)))
  }

  async fn add_fault(&self, request: Request<AddFaultRequest>) -> Result<Response<AddFaultResponse>, Status> {
    let caller = self.operator(&request, "actors cannot change a run's chaos")?;
    let request = request.into_inner();
    let added = self.runs.add_fault(&request.run_id, &request.name, &request.fault_yaml, caller.principal()).await?;
    Ok(Response::new(AddFaultResponse {
      run_id: added.run,
      warnings: added.warnings,
    }))
  }

  async fn list_faults(&self, request: Request<ListFaultsRequest>) -> Result<Response<ListFaultsResponse>, Status> {
    self.operator(&request, "actors cannot read a run's chaos")?;
    let faults = self.runs.faults(&request.into_inner().run_id).await?;
    Ok(Response::new(domain::proto_faults(faults)))
  }

  async fn remove_fault(&self, request: Request<RemoveFaultRequest>) -> Result<Response<RemoveFaultResponse>, Status> {
    let caller = self.operator(&request, "actors cannot change a run's chaos")?;
    let request = request.into_inner();
    let run_id = self.runs.remove_fault(&request.run_id, &request.name, caller.principal()).await?;
    Ok(Response::new(RemoveFaultResponse { run_id }))
  }
}

#[cfg(test)]
#[path = "run_test.rs"]
mod tests;
