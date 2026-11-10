//! `needy.v1` adapters. Each converts requests to domain types, calls the
//! daemon's trait, and converts the result or error back. None of them
//! decides anything about the run; [`Caller`](crate::control::Caller)
//! only scopes what an actor may name.

pub mod guest;
pub mod health;
pub mod process;
pub mod run;
pub mod runner;

use std::pin::Pin;

use tokio_stream::Stream;
use tonic::Status;

use crate::control::Caller;

/// A server-streaming response.
pub type Streamed<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send>>;

// NOTICE: 1 MiB chunks stay under gRPC's default 4 MiB message limit.
const CHUNK: usize = 1 << 20;

/// `bytes` as a stream of messages of at most 1 MiB each.
fn chunked<T: Send + 'static>(bytes: Vec<u8>, message: impl Fn(Vec<u8>) -> T) -> Streamed<T> {
  let messages: Vec<Result<T, Status>> = bytes.chunks(CHUNK).map(|c| Ok(message(c.to_vec()))).collect();
  Box::pin(tokio_stream::iter(messages))
}

/// Who made `request`: the caller the middleware identified by its bearer
/// token, or else the listener's own.
fn caller<T>(listener: &Caller, request: &tonic::Request<T>) -> Caller {
  request.extensions().get::<Caller>().cloned().unwrap_or_else(|| listener.clone())
}
