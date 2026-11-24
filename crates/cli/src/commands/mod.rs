//! One module per command. Each owns its `Args` and
//! `async fn run(args, ctx) -> anyhow::Result<i32>`, where the result is the
//! exit code.

pub mod client;
pub mod config;
pub mod daemon;
pub mod fs;
pub mod image;
pub mod proc;
pub mod profile;
pub mod run;
pub mod runner;
pub mod shell;

/// Parses a `KEY=VALUE` environment variable argument for clap `value_parser`.
pub(crate) fn parse_env(text: &str) -> Result<(String, String), String> {
  let (key, value) = text.split_once('=').ok_or("an environment entry must be KEY=VALUE")?;
  if key.is_empty() || key.contains('\0') || value.contains('\0') {
    return Err("an environment entry needs a nonempty key and no NUL bytes".into());
  }
  Ok((key.into(), value.into()))
}
