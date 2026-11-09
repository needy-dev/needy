//! `needy report`: a markdown report of a session from the trace.

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use clap::ValueEnum;

use crate::cli::Context;

#[derive(Clone, Copy, ValueEnum)]
enum Level {
  Summary,
  Detailed,
}

#[derive(clap::Args)]
pub struct Args {
  /// Session ID prefix, or `latest`.
  #[arg(default_value = "latest")]
  session: String,
  #[arg(long, value_enum, default_value = "summary")]
  level: Level,
  /// Write the report to this file instead of stdout.
  #[arg(long)]
  output: Option<PathBuf>,
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  // NOTICE: the report reads the trace database directly, so it needs the
  // daemon's state directory on this machine.
  let db = ctx.paths()?.state.join("trace.db");
  if !db.exists() {
    anyhow::bail!("no trace database at {}; is NEEDY_STATE_DIR the daemon's?", db.display());
  }
  let text = needy_core::report::render(&db, &args.session, matches!(args.level, Level::Detailed)).await?;
  match args.output {
    Some(path) => std::fs::write(&path, text).with_context(|| format!("cannot write {}", path.display()))?,
    None => print!("{text}"),
  }
  Ok(0)
}
