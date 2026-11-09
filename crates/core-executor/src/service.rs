//! The process service of an executor: runs processes on this machine.

use std::pin::Pin;

use futures_util::Stream;
use needy_api_proto::v1::process_service_server::ProcessService;
use needy_api_proto::v1::{
    ExecExited, ExecRequest, ExecResponse, ExecStarted, KillProcessRequest, KillProcessResponse,
    ListProcessesRequest, ListProcessesResponse, ProcessInfo, exec_request, exec_response,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

use crate::prochost::{Exit, Host, Input, Output, Proc, Spec};

pub type ExecStream = Pin<Box<dyn Stream<Item = Result<ExecResponse, Status>> + Send>>;

#[derive(Clone)]
pub struct ExecutorService {
    pub host: Host,
    pub name: String,
}

pub fn exited(exit: &Exit) -> ExecResponse {
    ExecResponse {
        payload: Some(exec_response::Payload::Exited(ExecExited {
            code: exit.code,
            signal: exit.signal.clone(),
        })),
    }
}

/// Streams a process to one client until it exits or the client leaves. A
/// client that leaves a process it did not detach takes the process down.
pub fn stream_process(
    proc: std::sync::Arc<Proc>,
    mut inbound: Streaming<ExecRequest>,
    started: ExecStarted,
    replay: bool,
    detach: bool,
) -> ExecStream {
    let (tx, rx) = mpsc::channel::<Result<ExecResponse, Status>>(64);
    let input_proc = proc.clone();
    tokio::spawn(async move {
        while let Ok(Some(message)) = inbound.message().await {
            match message.payload {
                Some(exec_request::Payload::Stdin(bytes)) => input_proc.send(Input::Stdin(bytes)),
                Some(exec_request::Payload::StdinEof(_)) => input_proc.send(Input::Eof),
                Some(exec_request::Payload::Signal(sig)) => input_proc.send(Input::Signal(sig)),
                Some(exec_request::Payload::Resize(size)) => {
                    input_proc.send(Input::Resize(size.rows as u16, size.cols as u16))
                }
                _ => {}
            }
        }
    });
    tokio::spawn(async move {
        let send = |event| {
            let tx = tx.clone();
            async move { tx.send(Ok(event)).await.is_ok() }
        };
        let mut alive = send(ExecResponse {
            payload: Some(exec_response::Payload::Started(started)),
        })
        .await;
        let (past, mut live) = proc.subscribe(replay).await;
        for event in past {
            if !alive {
                break;
            }
            alive = send(to_response(&event)).await;
            if matches!(event, Output::Exit(_)) {
                return;
            }
        }
        while alive {
            let Some(event) = live.recv().await else {
                break;
            };
            let done = matches!(event, Output::Exit(_));
            alive = send(to_response(&event)).await;
            if done {
                return;
            }
        }
        if !alive && !detach && proc.exit().is_none() {
            let sig = if proc.tty { 1 } else { 15 };
            proc.send(Input::Signal(sig));
        }
    });
    Box::pin(ReceiverStream::new(rx))
}

fn to_response(event: &Output) -> ExecResponse {
    match event {
        Output::Stdout(b) => ExecResponse {
            payload: Some(exec_response::Payload::Stdout(b.to_vec())),
        },
        Output::Stderr(b) => ExecResponse {
            payload: Some(exec_response::Payload::Stderr(b.to_vec())),
        },
        Output::Exit(exit) => exited(exit),
    }
}

pub fn info(proc: &Proc, executor: &str) -> ProcessInfo {
    let exit = proc.exit();
    ProcessInfo {
        process_id: proc.id.clone(),
        argv: proc.argv.clone(),
        executor: executor.to_string(),
        pid: proc.pid,
        running: exit.is_none(),
        exited: exit.map(|e| ExecExited {
            code: e.code,
            signal: e.signal,
        }),
        tty: proc.tty,
        started_at_ms: proc.started_at_ms,
        session_id: proc.session_id.clone(),
    }
}

#[tonic::async_trait]
impl ProcessService for ExecutorService {
    type ExecStream = ExecStream;

    async fn exec(
        &self,
        request: Request<Streaming<ExecRequest>>,
    ) -> Result<Response<Self::ExecStream>, Status> {
        let mut inbound = request.into_inner();
        let first = inbound
            .message()
            .await?
            .ok_or_else(|| Status::invalid_argument("empty request stream"))?;
        match first.payload {
            Some(exec_request::Payload::Start(start)) => {
                let tty = start
                    .tty
                    .map(|t| (t.rows.max(1) as u16, t.cols.max(1) as u16));
                let spec = Spec {
                    argv: start.argv,
                    cwd: start.cwd,
                    env: start.env.into_iter().collect(),
                    clear_env: start.clear_env,
                    tty,
                    no_stdin: start.no_stdin,
                    session_id: start.session_id,
                };
                let proc = self
                    .host
                    .spawn(spec)
                    .map_err(|e| Status::failed_precondition(format!("{e:#}")))?;
                let started = ExecStarted {
                    process_id: proc.id.clone(),
                    executor: self.name.clone(),
                    pid: proc.pid,
                    argv: proc.argv.clone(),
                    cwd: String::new(),
                };
                Ok(Response::new(stream_process(
                    proc,
                    inbound,
                    started,
                    false,
                    start.detach,
                )))
            }
            Some(exec_request::Payload::Attach(attach)) => {
                let proc = self
                    .host
                    .get(&attach.process_id)
                    .ok_or_else(|| Status::not_found("no such process"))?;
                let started = ExecStarted {
                    process_id: proc.id.clone(),
                    executor: self.name.clone(),
                    pid: proc.pid,
                    argv: proc.argv.clone(),
                    cwd: String::new(),
                };
                // An attached client never owns the process.
                Ok(Response::new(stream_process(
                    proc,
                    inbound,
                    started,
                    attach.replay,
                    true,
                )))
            }
            _ => Err(Status::invalid_argument(
                "the first request must start or attach",
            )),
        }
    }

    async fn list_processes(
        &self,
        request: Request<ListProcessesRequest>,
    ) -> Result<Response<ListProcessesResponse>, Status> {
        let session = request.into_inner().session_id;
        let processes = self
            .host
            .list()
            .iter()
            .filter(|p| session.is_empty() || p.session_id == session)
            .map(|p| info(p, &self.name))
            .collect();
        Ok(Response::new(ListProcessesResponse { processes }))
    }

    async fn kill_process(
        &self,
        request: Request<KillProcessRequest>,
    ) -> Result<Response<KillProcessResponse>, Status> {
        let request = request.into_inner();
        let proc = self
            .host
            .get(&request.process_id)
            .ok_or_else(|| Status::not_found("no such process"))?;
        proc.send(Input::Signal(if request.signal == 0 {
            15
        } else {
            request.signal
        }));
        Ok(Response::new(KillProcessResponse {}))
    }
}
