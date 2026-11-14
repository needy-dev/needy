//! One executor process, explicitly outside a run when creating it.

pub(crate) mod attach;
pub(crate) mod exec;
mod kill;
mod ps;

use crate::cli::Context;
use anyhow::Result;
use clap::Subcommand;

#[derive(clap::Args)]
pub struct Args {
  #[command(subcommand)]
  command: Command,
}

#[derive(Subcommand)]
enum Command {
  /// Execute outside a run on an explicit host, runner or pooled executor.
  Exec(exec::Args),
  /// List visible processes; optionally filter by run or runner.
  Ls(ps::Args),
  /// Reattach to a process and replay its recent output.
  Attach(attach::Args),
  /// Send a signal to one process.
  Kill(kill::Args),
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  match args.command {
    Command::Exec(args) => exec::run(args, ctx).await,
    Command::Ls(args) => ps::run(args, ctx).await,
    Command::Attach(args) => attach::run(args, ctx).await,
    Command::Kill(args) => kill::run(args, ctx).await,
  }
}
