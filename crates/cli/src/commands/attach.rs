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
  let attach = ExecAttach {
    process_id: args.process_id,
    replay: !args.no_replay,
    read_only: args.read_only,
    ..Default::default()
  };
  relay(&connection, attach, &args.detach_keys).await
}

/// Attaches this terminal to a process or run until it exits or the user types
/// `detach_keys`; the exit code as `needy exec` returns it.
pub(crate) async fn relay(connection: &needy_api_client::Client, mut attach: ExecAttach, detach_keys: &str) -> Result<i32> {
  let detach_keys = relay::DetachKeys::parse(detach_keys)?;
  let tty = relay::interactive();
  // The process's terminal takes this client's size, so a full-screen
  // program redraws for it, unless the client only watches.
  attach.window = tty.then(relay::window);
  let read_only = attach.read_only;
  let run = attach.run_id.clone();
  let stream = ExecStream::attach(connection, attach).await?;
  let options = relay::Options {
    tty,
    stdin: true,
    read_only,
    detach_keys: detach_keys.filter(|_| tty),
  };
  match relay::run(stream, options).await {
    Ok(relay::Ended::Detached { .. }) if !run.is_empty() => {
      crate::output::after_terminal(format!("detached from run {run}; `needy run attach {run}` returns to it"));
      Ok(0)
    }
    ended => super::exec::status(ended),
  }
}
