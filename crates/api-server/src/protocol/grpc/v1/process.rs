//! `ProcessService` over [`Processes`]: the first request opens the exec,
//! the rest of the stream is its input.

use std::sync::Arc;

use needy_api_proto::v1::process_service_server::ProcessService;
use needy_api_proto::v1::{
  ExecRequest, ExecResponse, KillProcessRequest, KillProcessResponse, ListProcessesRequest, ListProcessesResponse, exec_request,
};
use tokio_stream::StreamExt as _;
use tonic::{Request, Response, Status, Streaming};

use super::Streamed;
use crate::control::{Caller, ControlError, ExecOpen, Inputs, Processes, WindowSize};
use crate::protocol::{domain, status};

pub struct Adapter<P> {
  pub processes: Arc<P>,
  pub caller: Caller,
}

impl<P> Adapter<P> {
  /// The run a call acts on: an actor's own, or the one the request
  /// names.
  fn run(&self, named: String) -> Result<String, Status> {
    match &self.caller {
      Caller::Actor { run } => Ok(run.clone()),
      Caller::Cli => Ok(named),
      Caller::Runner { .. } => Err(ControlError::denied("runners cannot run processes here").into()),
    }
  }

  /// The runner a call names; an actor's commands are placed by its
  /// run's rules and may not name one.
  fn runner(&self, named: String) -> Result<String, Status> {
    match &self.caller {
      Caller::Actor { .. } if !named.is_empty() => Err(ControlError::denied("an actor cannot name a runner").into()),
      _ => Ok(named),
    }
  }
}

/// The caller's requests after the first, as input. A client stream that
/// fails ends the input, like one that closes.
fn inputs(inbound: Streaming<ExecRequest>) -> Inputs {
  let inputs = inbound
    .map_while(|message| match message {
      Ok(message) => Some(message),
      Err(status) => {
        tracing::debug!("exec: client stream: {status}");
        None
      }
    })
    .filter_map(|message| message.payload.and_then(domain::exec_input));
  Box::pin(inputs)
}

#[tonic::async_trait]
impl<P: Processes> ProcessService for Adapter<P> {
  type ExecStream = Streamed<ExecResponse>;

  async fn exec(&self, request: Request<Streaming<ExecRequest>>) -> Result<Response<Self::ExecStream>, Status> {
    let mut inbound = request.into_inner();
    let first = inbound.message().await?.ok_or_else(|| ControlError::Invalid("empty request stream".into()))?;
    let open = match first.payload {
      Some(exec_request::Payload::Start(start)) => {
        let mut spec = domain::exec_spec(start).map_err(ControlError::from)?;
        spec.run = self.run(spec.run)?;
        spec.runner = self.runner(spec.runner)?;
        ExecOpen::Start(spec)
      }
      Some(exec_request::Payload::Attach(attach)) => ExecOpen::Attach {
        process: attach.process_id,
        replay: attach.replay,
        window: attach.window.map(|w| WindowSize {
          rows: w.rows,
          cols: w.cols,
        }),
      },
      _ => return Err(ControlError::Invalid("the first request must start or attach".into()).into()),
    };
    let outputs = self.processes.exec(open, inputs(inbound)).await?;
    let responses = outputs.map(|output| output.map(domain::proto_exec_output).map_err(status::status));
    Ok(Response::new(Box::pin(responses)))
  }

  async fn list_processes(&self, request: Request<ListProcessesRequest>) -> Result<Response<ListProcessesResponse>, Status> {
    let request = request.into_inner();
    let run = self.run(request.run_id)?;
    let runner = self.runner(request.runner)?;
    let processes = self.processes.list(&run, &runner).await?.into_iter().map(domain::proto_process_info).collect();
    Ok(Response::new(ListProcessesResponse { processes }))
  }

  async fn kill_process(&self, request: Request<KillProcessRequest>) -> Result<Response<KillProcessResponse>, Status> {
    let request = request.into_inner();
    self.processes.kill(&request.process_id, &request.signal).await?;
    Ok(Response::new(KillProcessResponse {}))
  }
}

#[cfg(test)]
#[path = "process_test.rs"]
mod tests;
