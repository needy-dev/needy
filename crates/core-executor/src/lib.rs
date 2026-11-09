//! Where commands run: the process host that owns processes and terminals,
//! the process service an executor serves, and the client that streams a
//! process to a terminal or a tool.

pub mod client;
pub mod prochost;
pub mod service;
