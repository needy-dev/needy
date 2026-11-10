//! `needy attach`: reconnect to a process: a terminal's current screen, or
//! the recent output of pipes, then what follows.

use anyhow::Result;
use needy_api_client::exec::ExecStream;
use needy_api_client::proto::v1::ExecAttach;
use needy_core_executor::relay;

use crate::cli::Context;

#[derive(clap::Args)]
pub struct Args {
  process_id: String,
  /// Do not replay buffered output or draw the current screen.
  #[arg(long)]
  no_replay: bool,
  /// Watch only: input is not sent, and the window size stays another
  /// client's.
  #[arg(long)]
  read_only: bool,
  /// Keys that leave the process running, as Docker writes them
  /// (`ctrl-p,ctrl-q`); empty turns detaching off.
  #[arg(long, default_value = relay::DEFAULT_DETACH_KEYS)]
  detach_keys: String,
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  let connection = crate::cli::connect(&ctx.endpoint()?).await?;
  relay(&connection, &args.process_id, args.no_replay, args.read_only, &args.detach_keys).await
}

/// Attaches this terminal to `process` until it exits or the user types
/// `detach_keys`; the exit code as `needy exec` returns it.
pub(crate) async fn relay(
  connection: &needy_api_client::Client,
  process: &str,
  no_replay: bool,
  read_only: bool,
  detach_keys: &str,
) -> Result<i32> {
  let detach_keys = relay::DetachKeys::parse(detach_keys)?;
  let tty = relay::interactive();
  // The process's terminal takes this client's size, so a full-screen
  // program redraws for it, unless the client only watches.
  let attach = ExecAttach {
    process_id: process.to_string(),
    replay: !no_replay,
    window: tty.then(relay::window),
    read_only,
  };
  let stream = ExecStream::attach(connection, attach).await?;
  let options = relay::Options {
    tty,
    stdin: true,
    read_only,
    detach_keys: detach_keys.filter(|_| tty),
  };
  super::exec::status(relay::run(stream, options).await)
}
