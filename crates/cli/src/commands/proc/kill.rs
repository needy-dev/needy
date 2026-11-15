//! `needy kill`: send a signal to a process.

use anyhow::{Result, anyhow};
use needy_api_client::proto::v1::KillProcessRequest;

use crate::cli::Context;
use crate::output;

#[derive(clap::Args)]
pub struct Args {
  process_id: String,
  /// Signal name, for example SIGKILL; a number is the host's signal.
  #[arg(short, long, default_value = "SIGTERM")]
  signal: String,
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  let signal = needy_core_executor::signals::parse(&args.signal).ok_or_else(|| anyhow!("unknown signal {}", args.signal))?;
  let progress = output::Progress::new(ctx.connection_feedback(), "Sending signal request...");
  let connection = ctx.connect().await?;
  connection
    .processes()
    .kill_process(KillProcessRequest {
      process_id: args.process_id.clone(),
      signal,
    })
    .await
    .map_err(needy_api_client::Error::from)?;
  progress.finish();
  output::acknowledge(&format!("Requested {} for process {}.", args.signal, args.process_id))?;
  Ok(0)
}
