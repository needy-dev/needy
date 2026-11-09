//! `needy report`: a markdown report of a session from the trace. Events are
//! read with the SQLite sink's reader, decoded into `needy_core::event` values
//! and rendered by `needy_core::report`.

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use clap::ValueEnum;
use needy_core::event::{Event, Recorded};
use needy_core::report;

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
  let loaded = needy_trace::sink::sqlite::load(&db, &args.session).await?;
  let events = loaded
    .events
    .iter()
    .map(|stored| {
      let event = Event::decode(&stored.kind, stored.version, stored.payload.get())
        .with_context(|| format!("session {}: event {} ({}) cannot be read", loaded.session, stored.seq, stored.kind))?;
      Ok(Recorded {
        time_ms: stored.time_ms,
        event,
      })
    })
    .collect::<Result<Vec<_>>>()?;
  let level = match args.level {
    Level::Summary => report::Level::Summary,
    Level::Detailed => report::Level::Detailed,
  };
  let text = report::render(&loaded.session, &events, level);
  match args.output {
    Some(path) => std::fs::write(&path, text).with_context(|| format!("cannot write {}", path.display()))?,
    None => print!("{text}"),
  }
  Ok(0)
}
