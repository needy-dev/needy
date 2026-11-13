//! Shared helpers that depend on neither the platform nor an async runtime:
//! account files (passwd, group, shadow), include and exclude filters, tar
//! archives, process exit status and signal names, values derived from a
//! seed, SFTP packet framing, and wall-clock time.

pub mod account;
pub mod archive;
pub mod filter;
pub mod process;
pub mod seed;
pub mod sftp;
#[cfg(all(feature = "testing", unix))]
pub mod testing;
pub mod time;
