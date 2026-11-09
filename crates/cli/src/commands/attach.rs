//! `needy attach`: reconnect to a detached process, replaying recent output.

use anyhow::Result;
use needy_api_client::exec::ExecStream;
use needy_core_executor::relay;

use crate::cli::Context;

#[derive(clap::Args)]
pub struct Args {
  process_id: String,
  /// Do not replay buffered output.
  #[arg(long)]
  no_replay: bool,
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  let connection = crate::cli::connect(&ctx.endpoint()?).await?;
  let stream = ExecStream::attach(&connection, &args.process_id, !args.no_replay).await?;
  let tty = relay::interactive();
  super::exec::status(relay::run(stream, tty, true).await)
}
