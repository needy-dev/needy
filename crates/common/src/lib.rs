//! Shared helpers that depend on neither the platform nor an async runtime:
//! globs and durations, include and exclude filters, tar archives, and process
//! exit status and signal names.

pub mod archive;
pub mod filter;
pub mod process;
pub mod util;
