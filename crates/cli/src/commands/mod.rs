//! One module per command. Each owns its `Args` and
//! `async fn run(args, ctx) -> anyhow::Result<i32>`, where the result is the
//! exit code.

pub mod attach;
pub mod chaos;
pub mod daemon;
pub mod exec;
pub mod fs;
pub mod image;
pub mod kill;
pub mod pair;
pub mod profile;
pub mod ps;
pub mod report;
pub mod run;
pub mod runner;
