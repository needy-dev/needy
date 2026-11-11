//! `needy.v1` adapters. Each converts requests to domain types, calls the
//! daemon's trait, and converts the result or error back. None of them
//! decides anything about the run; [`Caller`](crate::control::Caller)
//! only scopes what an actor may name.

pub mod client;
pub mod guest;
pub mod health;
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

/// Refuses a call that administers the control plane (`what`, such as
/// "creating join tokens") unless the operator makes it: the control token,
/// or the control socket without a client token. A paired client drives runs
/// but does not change who may reach the control plane (owner decision,
/// 2026-11-11). `others` is the refusal for actors and runners.
fn administer<T>(listener: &Caller, request: &tonic::Request<T>, what: &str, others: &str) -> Result<(), Status> {
  match caller(listener, request) {
    Caller::Operator => Ok(()),
    Caller::Client { name, .. } => Err(ControlError::denied(format!("{what} needs the control token; paired client {name} may not")).into()),
    Caller::Actor { .. } | Caller::Runner { .. } => Err(ControlError::denied(others).into()),
  }
}

/// Refuses a call that reads the control plane's credentials (`what`, such
/// as "listing join tokens") unless the operator makes it on the control
/// host itself: on a Unix socket, which only the control host's own users
/// reach. The control token over TCP and a paired client are refused (owner
/// decision, 2026-11-12). `others` is the refusal for actors and runners.
fn administer_locally<T>(listener: &Caller, request: &tonic::Request<T>, what: &str, others: &str) -> Result<(), Status> {
  match caller(listener, request) {
    Caller::Operator if over_unix_socket(request) => Ok(()),
    Caller::Operator => Err(ControlError::denied(format!("{what} works only on the control host's Unix socket, not over TCP")).into()),
    Caller::Client { name, .. } => {
      Err(ControlError::denied(format!("{what} works only on the control host's Unix socket; paired client {name} may not")).into())
    }
    Caller::Actor { .. } | Caller::Runner { .. } => Err(ControlError::denied(others).into()),
  }
}

/// Whether `request` came over a Unix socket: tonic puts the connection's
/// `UdsConnectInfo` into the extensions of every request it serves on one
/// (`tonic-0.14.9/src/transport/server/mod.rs`, `ConnectInfoLayer`), and
/// `TcpConnectInfo` on TCP.
fn over_unix_socket<T>(request: &tonic::Request<T>) -> bool {
  #[cfg(unix)]
  {
    request.extensions().get::<tonic::transport::server::UdsConnectInfo>().is_some()
  }
  #[cfg(not(unix))]
  {
    let _ = request;
    false
  }
}
