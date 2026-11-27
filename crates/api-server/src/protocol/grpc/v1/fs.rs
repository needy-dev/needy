//! `FsService` over [`Fs`].

use std::sync::Arc;

use needy_api_proto::v1::fs_service_server::FsService;
use needy_api_proto::v1::{SessionRequest, SessionResponse};
use tokio_stream::StreamExt as _;
use tonic::{Request, Response, Status, Streaming};

use super::{Streamed, caller};
use crate::control::{Caller, ControlError, Fs};

pub struct Adapter<F> {
  pub fs: Arc<F>,
  pub caller: Caller,
}

#[tonic::async_trait]
impl<F: Fs> FsService for Adapter<F> {
  type SessionStream = Streamed<SessionResponse>;

  async fn session(&self, request: Request<Streaming<SessionRequest>>) -> Result<Response<Self::SessionStream>, Status> {
    let caller = caller(&self.caller, &request);
    if matches!(caller, Caller::Runner { .. }) {
      return Err(ControlError::role_denied("runners cannot open filesystem sessions").into());
    }
    let responses = self.fs.session(caller, Box::pin(request.into_inner())).await?;
    Ok(Response::new(Box::pin(responses.map(Ok))))
  }
}

#[cfg(test)]
#[path = "fs_test.rs"]
mod tests;
