//! Serving the Needy daemon API.
//!
//! The daemon implements the domain traits in [`control`]; nothing it sees
//! is generated code. This crate does the rest:
//!
//! - [`server`]: binds every listener (Unix, TCP, a vsock's Unix socket),
//!   then serves the services one [`control::Caller`] reaches.
//! - [`protocol`]: the gRPC adapters, proto <-> domain mappings, and the one
//!   table from [`control::ControlError`] to a status.
//! - [`middleware`]: bearer authentication.
//! - [`termination`]: termination signals as cancellation.

pub mod control;
pub mod middleware;
pub mod protocol;
pub mod server;
pub mod termination;

#[cfg(test)]
mod testing;

pub use needy_api_proto as proto;
pub use protocol::grpc::v1::health::Health;
pub use server::{Listen, Server};
