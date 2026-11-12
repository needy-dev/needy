//! Where commands run.
//!
//! - [`account`]: the profile's user in a container's account files, for
//!   both `linux-container` drivers.
//! - [`process`]: the process host that owns processes and terminals on this
//!   machine, independent of the clients streaming them.
//! - [`service`]: `ExecutorService`, the process service one machine serves.
//! - [`driver`]: executor drivers that create and destroy instances
//!   (`native`, `linux-container`).
//! - [`engine`]: the Docker-compatible engine client both `linux-container`
//!   drivers use, this crate's and the actor's.
//! - [`relay`]: the client side, which relays an exec stream to the local
//!   terminal or pipes.
//! - [`signals`]: signal names, this platform's numbers and shell exit codes.

pub mod account;
pub mod driver;
pub mod engine;
pub mod process;
pub mod relay;
pub mod service;
pub mod signals;
