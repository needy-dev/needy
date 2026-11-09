//! Where commands run.
//!
//! - [`process`]: the process host that owns processes and terminals on this
//!   machine, independent of the clients streaming them.
//! - [`service`]: `ExecutorService`, the process service one machine serves.
//! - [`provider`]: executor runtimes that create and destroy instances
//!   (`native`, `remote`, `docker`).
//! - [`relay`]: the client side, which relays an exec stream to the local
//!   terminal or pipes.
//! - [`signals`]: signal names, this platform's numbers and shell exit codes.

pub mod process;
pub mod provider;
pub mod relay;
pub mod service;
pub mod signals;
