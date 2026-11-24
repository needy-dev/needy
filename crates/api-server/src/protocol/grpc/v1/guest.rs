//! `GuestService` over [`Guest`]: served only on an actor's listener, for
//! the run it is bound to.

use std::sync::Arc;

use needy_api_proto::v1::guest_service_control_response::Message;
use needy_api_proto::v1::guest_service_server::GuestService;
use needy_api_proto::v1::{
  BridgeRequest, BridgeResponse, FetchFilesRequest, FetchFilesResponse, FetchLayerRequest, FetchLayerResponse, GetConfigRequest,
  GetConfigResponse, GracefulStop, GuestServiceControlRequest, GuestServiceControlResponse, PushEventsRequest, PushEventsResponse,
  PushFilesRequest, PushFilesResponse, PushLayerRequest, PushLayerResponse, ReportExitRequest, ReportExitResponse, RouteList,
  TerminalTaskRequest, TerminalTaskResponse, terminal_task_request,
};
use needy_common::process::Exit;
use needy_core::event::Event;
use tonic::{Request, Response, Status, Streaming};

use super::{CHUNK, Streamed, chunked};
use tokio_stream::StreamExt as _;

use crate::control::{Caller, Chunks, ControlError, Guest, GuestControl};

pub struct Adapter<G> {
  pub guest: Arc<G>,
  pub caller: Caller,
}

impl<G> Adapter<G> {
  fn run<T>(&self, request: &Request<T>) -> Result<String, Status> {
    match super::caller(&self.caller, request) {
      Caller::Actor { run } => Ok(run),
      Caller::Operator | Caller::Client { .. } | Caller::Runner { .. } => Err(ControlError::role_denied("not an actor listener").into()),
    }
  }
}

#[tonic::async_trait]
impl<G: Guest> GuestService for Adapter<G> {
  async fn get_config(&self, request: Request<GetConfigRequest>) -> Result<Response<GetConfigResponse>, Status> {
    let run = self.run(&request)?;
    let config = self.guest.config(&run).await?;
    Ok(Response::new(config.into()))
  }

  async fn push_events(&self, request: Request<Streaming<PushEventsRequest>>) -> Result<Response<PushEventsResponse>, Status> {
    let run = self.run(&request)?;
    let mut stream = request.into_inner();
    while let Some(message) = stream.message().await? {
      let Some(pushed) = message.event else {
        continue;
      };
      // The guest build's schema version must be one this daemon reads.
      let event = Event::decode(&pushed.kind, pushed.version, &pushed.payload)
        .map_err(|e| ControlError::Invalid(format!("guest event {}: {e}", pushed.kind)))?;
      self.guest.event(&run, pushed.time_ms, event).await?;
    }
    Ok(Response::new(PushEventsResponse {}))
  }

  async fn report_exit(&self, request: Request<ReportExitRequest>) -> Result<Response<ReportExitResponse>, Status> {
    let run = self.run(&request)?;
    let exit = request.into_inner();
    let exit = Exit {
      code: exit.code,
      signal: exit.signal,
    };
    self.guest.exit(&run, exit).await?;
    Ok(Response::new(ReportExitResponse {}))
  }

  type FetchFilesStream = Streamed<FetchFilesResponse>;

  async fn fetch_files(&self, request: Request<FetchFilesRequest>) -> Result<Response<Self::FetchFilesStream>, Status> {
    let run = self.run(&request)?;
    let archive = self.guest.fetch_files(&run, &request.into_inner().name).await?;
    Ok(Response::new(chunked(archive, |data| FetchFilesResponse { data })))
  }

  async fn push_files(&self, request: Request<Streaming<PushFilesRequest>>) -> Result<Response<PushFilesResponse>, Status> {
    let run = self.run(&request)?;
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
    self.guest.push_files(&run, &name, archive).await?;
    Ok(Response::new(PushFilesResponse {}))
  }

  async fn push_layer(&self, request: Request<Streaming<PushLayerRequest>>) -> Result<Response<PushLayerResponse>, Status> {
    let run = self.run(&request)?;
    self.guest.push_layer(&run, layer_parts(request.into_inner())).await?;
    Ok(Response::new(PushLayerResponse {}))
  }

  type ControlStream = Streamed<GuestServiceControlResponse>;

  async fn control(&self, request: Request<GuestServiceControlRequest>) -> Result<Response<Self::ControlStream>, Status> {
    let run = self.run(&request)?;
    let controls = self.guest.control(&run).await?;
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

  type FetchLayerStream = Streamed<FetchLayerResponse>;

  async fn fetch_layer(&self, request: Request<FetchLayerRequest>) -> Result<Response<Self::FetchLayerStream>, Status> {
    let run = self.run(&request)?;
    let chunks = self.guest.fetch_layer(&run, &request.into_inner().digest).await?;
    // A read may return more than one message carries; it is split, never
    // joined, since a layer can be hundreds of megabytes.
    let messages = futures_util::StreamExt::flat_map(chunks, |read| {
      let messages: Vec<Result<FetchLayerResponse, Status>> = match read {
        Ok(bytes) => bytes
          .chunks(CHUNK)
          .map(|data| {
            Ok(FetchLayerResponse {
              data: data.to_vec(),
            })
          })
          .collect(),
        Err(e) => vec![Err(e.into())],
      };
      tokio_stream::iter(messages)
    });
    Ok(Response::new(Box::pin(messages)))
  }

  type BridgeStream = Streamed<BridgeResponse>;

  async fn bridge(&self, request: Request<Streaming<BridgeRequest>>) -> Result<Response<Self::BridgeStream>, Status> {
    let run = self.run(&request)?;
    let outbound = self.guest.bridge(&run, Box::pin(request.into_inner())).await?;
    Ok(Response::new(Box::pin(outbound.map(Ok))))
  }

  type TerminalTaskStream = Streamed<TerminalTaskResponse>;

  async fn terminal_task(&self, request: Request<Streaming<TerminalTaskRequest>>) -> Result<Response<Self::TerminalTaskStream>, Status> {
    let run = self.run(&request)?;
    let mut inbound = request.into_inner();
    let Some(first) = inbound.message().await? else {
      return Err(ControlError::Invalid("the terminal task stream ended before naming a task".into()).into());
    };
    let Some(terminal_task_request::Payload::TaskId(task)) = first.payload else {
      return Err(ControlError::Invalid("the first message on a terminal task stream must be task_id".into()).into());
    };
    let outbound = self.guest.terminal_task(&run, task, Box::pin(inbound)).await?;
    Ok(Response::new(Box::pin(outbound.map(Ok))))
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
