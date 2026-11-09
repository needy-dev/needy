//! `needy kill`: send a signal to a process.

use anyhow::{Result, anyhow};
use needy_api_client::proto::v1::KillProcessRequest;

use crate::cli::Context;

#[derive(clap::Args)]
pub struct Args {
  process_id: String,
  /// Signal name, for example SIGKILL; a number is the host's signal.
  #[arg(short, long, default_value = "SIGTERM")]
  signal: String,
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  let signal = needy_core_executor::signals::parse(&args.signal).ok_or_else(|| anyhow!("unknown signal {}", args.signal))?;
  let connection = crate::cli::connect(&ctx.endpoint()?).await?;
  connection
    .processes()
    .kill_process(KillProcessRequest {
      process_id: args.process_id,
      signal,
    })
    .await
    .map_err(needy_api_client::Error::from)?;
  Ok(0)
}
