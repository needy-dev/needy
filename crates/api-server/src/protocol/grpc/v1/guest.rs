//! `GuestService` over [`Guest`]: served only on an actor's listener, for
//! the run it is bound to.

use std::sync::Arc;

use needy_api_proto::v1::guest_service_control_response::Message;
use needy_api_proto::v1::guest_service_server::GuestService;
use needy_api_proto::v1::{
  FetchFilesRequest, FetchFilesResponse, GetConfigRequest, GetConfigResponse, GracefulStop, GuestServiceControlRequest,
  GuestServiceControlResponse, PushEventsRequest, PushEventsResponse, PushFilesRequest, PushFilesResponse, PushLayerRequest,
  PushLayerResponse, ReportExitRequest, ReportExitResponse, RouteList,
};
use needy_common::process::Exit;
use needy_core::event::Event;
use tonic::{Request, Response, Status, Streaming};

use super::{Streamed, chunked};
use tokio_stream::StreamExt as _;

use crate::control::{Caller, Chunks, ControlError, Guest, GuestControl};
use crate::protocol::domain;

pub struct Adapter<G> {
  pub guest: Arc<G>,
  pub caller: Caller,
}

impl<G> Adapter<G> {
  fn run(&self) -> Result<&str, Status> {
    match &self.caller {
      Caller::Actor { run } => Ok(run),
      Caller::Operator | Caller::Client { .. } | Caller::Runner { .. } => Err(ControlError::role_denied("not an actor listener").into()),
    }
  }
}

#[tonic::async_trait]
impl<G: Guest> GuestService for Adapter<G> {
  async fn get_config(&self, _: Request<GetConfigRequest>) -> Result<Response<GetConfigResponse>, Status> {
    let config = self.guest.config(self.run()?).await?;
    Ok(Response::new(domain::proto_guest_config(config)))
  }

  async fn push_events(&self, request: Request<Streaming<PushEventsRequest>>) -> Result<Response<PushEventsResponse>, Status> {
    let run = self.run()?;
    let mut stream = request.into_inner();
    while let Some(message) = stream.message().await? {
      let Some(pushed) = message.event else {
        continue;
      };
      // The guest build's schema version must be one this daemon reads.
      let event = Event::decode(&pushed.kind, pushed.version, &pushed.payload)
        .map_err(|e| ControlError::Invalid(format!("guest event {}: {e}", pushed.kind)))?;
      self.guest.event(run, pushed.time_ms, event).await?;
    }
    Ok(Response::new(PushEventsResponse {}))
  }

  async fn report_exit(&self, request: Request<ReportExitRequest>) -> Result<Response<ReportExitResponse>, Status> {
    let run = self.run()?;
    let exit = request.into_inner();
    let exit = Exit {
      code: exit.code,
      signal: exit.signal,
    };
    self.guest.exit(run, exit).await?;
    Ok(Response::new(ReportExitResponse {}))
  }

  type FetchFilesStream = Streamed<FetchFilesResponse>;

  async fn fetch_files(&self, request: Request<FetchFilesRequest>) -> Result<Response<Self::FetchFilesStream>, Status> {
    let run = self.run()?;
    let archive = self.guest.fetch_files(run, &request.into_inner().name).await?;
    Ok(Response::new(chunked(archive, |data| FetchFilesResponse { data })))
  }

  async fn push_files(&self, request: Request<Streaming<PushFilesRequest>>) -> Result<Response<PushFilesResponse>, Status> {
    let run = self.run()?;
    let mut stream = request.into_inner();
    // The first message names the mapping; the rest carry only data.
    let mut name = String::new();
    let mut archive = Vec::new();
    while let Some(message) = stream.message().await? {
      if name.is_empty() {
        name = message.name;
      }
      archive.extend(message.data);
    }
    self.guest.push_files(run, &name, archive).await?;
    Ok(Response::new(PushFilesResponse {}))
  }

  async fn push_layer(&self, request: Request<Streaming<PushLayerRequest>>) -> Result<Response<PushLayerResponse>, Status> {
    let run = self.run()?;
    self.guest.push_layer(run, layer_parts(request.into_inner())).await?;
    Ok(Response::new(PushLayerResponse {}))
  }

  type ControlStream = Streamed<GuestServiceControlResponse>;

  async fn control(&self, _: Request<GuestServiceControlRequest>) -> Result<Response<Self::ControlStream>, Status> {
    let controls = self.guest.control(self.run()?).await?;
    Ok(Response::new(Box::pin(controls.map(|control| {
      Ok(GuestServiceControlResponse {
        message: Some(match control {
          GuestControl::Routes(names) => Message::Routes(RouteList { names }),
          GuestControl::Stop { grace } => Message::Stop(GracefulStop {
            grace_ms: i64::try_from(grace.as_millis()).unwrap_or(i64::MAX),
          }),
        }),
      })
    }))))
  }
}

/// A layer upload's parts as they arrive, never joined, since a layer can be
/// hundreds of megabytes. A stream that ends before its last part, or
/// breaks, ends with an error, so the daemon never takes a truncated archive
/// for a whole one.
fn layer_parts(stream: Streaming<PushLayerRequest>) -> Chunks {
  Box::pin(futures_util::stream::unfold(Some(stream), |stream| async move {
    let mut stream = stream?;
    match stream.message().await {
      Ok(Some(message)) if message.last => Some((Ok(message.data), None)),
      Ok(Some(message)) => Some((Ok(message.data), Some(stream))),
      Ok(None) => Some((Err(ControlError::Invalid("the layer upload ended before its last part".into())), None)),
      Err(status) => Some((Err(ControlError::Unavailable(format!("the layer upload broke: {}", status.message()))), None)),
    }
  }))
}

#[cfg(test)]
#[path = "guest_test.rs"]
mod tests;
