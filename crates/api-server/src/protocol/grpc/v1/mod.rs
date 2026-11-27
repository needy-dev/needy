//! `needy.v1` adapters. Each converts requests to domain types, calls the
//! daemon's trait, and converts the result or error back. None of them
//! decides anything about the run; [`Caller`](crate::control::Caller)
//! only scopes what an actor may name.

pub mod client;
pub mod fs;
pub mod guest;
pub mod health;
pub mod image;
pub mod process;
pub mod run;
pub mod runner;

use std::pin::Pin;

use tokio_stream::Stream;
use tonic::Status;

use crate::control::{Caller, ControlError};

/// A server-streaming response.
pub type Streamed<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send>>;

// NOTICE: 1 MiB chunks stay under gRPC's default 4 MiB message limit.
const CHUNK: usize = 1 << 20;

/// `bytes` as a stream of messages of at most 1 MiB each, sliced lazily as the
/// stream is polled so the payload is not duplicated in memory up front.
fn chunked<T: Send + 'static>(mut bytes: Vec<u8>, message: impl Fn(Vec<u8>) -> T + Send + 'static) -> Streamed<T> {
  let mut offset = 0;
  let iter = std::iter::from_fn(move || {
    if offset >= bytes.len() {
      return None;
    }
    let chunk = if offset == 0 && bytes.len() <= CHUNK {
      offset = bytes.len();
      std::mem::take(&mut bytes)
    } else {
      let end = (offset + CHUNK).min(bytes.len());
      let chunk = bytes[offset..end].to_vec();
      offset = end;
      if offset >= bytes.len() {
        bytes = Vec::new();
      }
      chunk
    };
    Some(Ok(message(chunk)))
  });
  Box::pin(tokio_stream::iter(iter))
}

/// Who made `request`: the caller the middleware identified by its bearer
/// token, or else the listener's own.
fn caller<T>(listener: &Caller, request: &tonic::Request<T>) -> Caller {
  request.extensions().get::<Caller>().cloned().unwrap_or_else(|| listener.clone())
}

/// The caller of a call only an operator or a paired client may make.
fn operator<T>(listener: &Caller, request: &tonic::Request<T>, actor_refusal: &str, runner_refusal: &str) -> Result<Caller, Status> {
  match caller(listener, request) {
    caller @ (Caller::Operator | Caller::Client { .. }) => Ok(caller),
    Caller::Actor { .. } => Err(ControlError::role_denied(actor_refusal).into()),
    Caller::Runner { .. } => Err(ControlError::role_denied(runner_refusal).into()),
  }
}

/// Management authority is an explicit server grant, independent of transport.
fn administer<T>(listener: &Caller, request: &tonic::Request<T>, what: &str, others: &str) -> Result<(), Status> {
  match caller(listener, request) {
    Caller::Operator
    | Caller::Client {
      access: crate::control::ClientAccess::Admin,
      ..
    } => Ok(()),
    Caller::Client { name, .. } => Err(
      ControlError::denied(format!("{what} needs administrator access; paired client {name} may not"))
        .with_diagnostic("auth.admin_required", [("caller".into(), name), ("access".into(), "runs".into())])
        .into(),
    ),
    Caller::Actor { .. } | Caller::Runner { .. } => Err(ControlError::denied(others).with_diagnostic("auth.role_denied", []).into()),
  }
}

#[cfg(test)]
mod tests {
  use tokio_stream::StreamExt as _;

  use super::{CHUNK, chunked};

  #[tokio::test]
  async fn chunked_streams_empty_single_and_multi_chunk_payloads() {
    let empty: Vec<Vec<u8>> = chunked(Vec::new(), |b| b).map(Result::unwrap).collect().await;
    assert!(empty.is_empty());

    let small = vec![7u8; 42];
    let single: Vec<Vec<u8>> = chunked(small.clone(), |b| b).map(Result::unwrap).collect().await;
    assert_eq!(single, vec![small]);

    let large: Vec<u8> = (0..(2 * CHUNK + 13)).map(|i| (i % 251) as u8).collect();
    let chunks: Vec<Vec<u8>> = chunked(large.clone(), |b| b).map(Result::unwrap).collect().await;
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].len(), CHUNK);
    assert_eq!(chunks[1].len(), CHUNK);
    assert_eq!(chunks[2].len(), 13);
    assert_eq!(chunks.concat(), large);
  }
}
