//! `needy attach`: reconnect to a detached process, replaying recent output.

use anyhow::Result;
use needy_api_proto::v1::{ExecAttach, ExecRequest, exec_request};
use needy_core_executor::client;

use crate::cli::Context;

#[derive(clap::Args)]
pub struct Args {
  process_id: String,
  /// Do not replay buffered output.
  #[arg(long)]
  no_replay: bool,
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  let connection = client::connect(&ctx.endpoint()?).await?;
  let first = ExecRequest {
    payload: Some(exec_request::Payload::Attach(ExecAttach {
      process_id: args.process_id,
      replay: !args.no_replay,
    })),
  };
  let tty = client::stdin_is_tty() && client::stdout_is_tty();
  let exit = client::run_stream(connection, first, tty, true).await?;
  Ok(needy_core_executor::signals::shell_code(&exit))
}
