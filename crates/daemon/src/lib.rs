//! The daemon: listeners and authentication for every role, and the control
//! plane that composes sessions, actors and executors.

#[cfg(unix)]
pub mod control;
pub mod server;
