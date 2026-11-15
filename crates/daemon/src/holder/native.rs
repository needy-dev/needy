//! The native host's lifetime policy: run-owned processes outlive a control
//! stream; standalone `needy exec` retains its existing client-owned lifetime.

use futures_util::StreamExt as _;
use needy_api_client::proto::v1::{
  ExecRequest, KillProcessRequest, KillProcessResponse, ListProcessesRequest, ListProcessesResponse, exec_request,
  process_service_server::ProcessService,
};
use needy_core_executor::service::{ExecStream, ExecutorService};
use tonic::{Request, Response, Status, Streaming};

#[derive(Clone)]
pub(super) struct Service(pub ExecutorService);

#[tonic::async_trait]
impl ProcessService for Service {
  type ExecStream = ExecStream;

  async fn exec(&self, request: Request<Streaming<ExecRequest>>) -> Result<Response<ExecStream>, Status> {
    let mut incoming = request.into_inner();
    let first = incoming.next().await;
    let preserve = match first.as_ref().and_then(|request| request.as_ref().ok()).and_then(|request| request.payload.as_ref()) {
      Some(exec_request::Payload::Start(start)) => !start.run_id.is_empty(),
      Some(exec_request::Payload::LimitedStart(limited)) => limited.start.as_ref().is_some_and(|start| !start.run_id.is_empty()),
      Some(exec_request::Payload::Attach(attach)) => self.0.host.get(&attach.process_id).is_some_and(|process| !process.run_id.is_empty()),
      _ => false,
    };
    let inbound = Box::pin(futures_util::stream::iter(first).chain(incoming));
    let output = if preserve {
      self.0.open_preserving(inbound).await?
    } else {
      self.0.open(inbound).await?
    };
    Ok(Response::new(output))
  }

  async fn list_processes(&self, request: Request<ListProcessesRequest>) -> Result<Response<ListProcessesResponse>, Status> {
    self.0.list_processes(request).await
  }

  async fn kill_process(&self, request: Request<KillProcessRequest>) -> Result<Response<KillProcessResponse>, Status> {
    self.0.kill_process(request).await
  }
}
