//! The daemon: the control plane that composes sessions, actors and
//! executors, and the executor role. Listening, authentication and the wire
//! are `needy-api-server`'s; the daemon implements its domain traits.

#[cfg(unix)]
pub mod control;
pub mod executor;
