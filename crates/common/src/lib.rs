//! Shared helpers that depend on neither the platform nor an async runtime:
//! include and exclude filters, tar archives, process exit status and signal
//! names, and values derived from a seed.

pub mod archive;
pub mod filter;
pub mod process;
pub mod seed;
