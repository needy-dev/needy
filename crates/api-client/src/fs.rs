//! Workspace filesystem streams (`FsService.Session` and `RunnerService.FsTask`).

use needy_api_proto::v1::{FsTaskRequest, FsTaskResponse, SessionRequest, SessionResponse, fs_task_request};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::{Client, Error, Result};

// NOTICE: 64 requests in flight keep multiplexed FUSE operations ahead of the
// network without unbounded buffering.
pub(crate) const QUEUE: usize = 64;

/// Opens a bidirectional `FsService.Session` stream on `client`, carrying the
/// client's bearer token, custom headers, and optional `x-needy-run-id`.
pub async fn session(client: &Client) -> Result<(mpsc::Sender<SessionRequest>, tonic::Streaming<SessionResponse>)> {
  let (tx, rx) = mpsc::channel::<SessionRequest>(QUEUE);
  let responses = client.fs().session(ReceiverStream::new(rx)).await?.into_inner();
  Ok((tx, responses))
}

/// Opens a bidirectional `RunnerService.FsTask` stream on `client` for
/// `task_id`, enqueuing the initial `task_id` handshake frame before awaiting
/// the server's response stream.
pub async fn runner_task(client: &Client, task_id: u64) -> Result<(mpsc::Sender<FsTaskRequest>, tonic::Streaming<FsTaskResponse>)> {
  let (tx, rx) = mpsc::channel::<FsTaskRequest>(QUEUE);
  let named = FsTaskRequest {
    payload: Some(fs_task_request::Payload::TaskId(task_id)),
  };
  tx.send(named).await.map_err(|_| Error::Closed)?;
  let responses = client.runners().fs_task(ReceiverStream::new(rx)).await?.into_inner();
  Ok((tx, responses))
}
