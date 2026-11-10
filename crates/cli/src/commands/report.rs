//! `needy report`: a report of a run from the trace, as plain text for
//! people, markdown or JSON. Events are read with the SQLite sink's reader,
//! decoded into `needy_core::event` values and rendered by
//! `needy_core::report`; this command picks the format and the colors.

use std::io::IsTerminal;
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

#[derive(Clone, Copy, ValueEnum)]
enum Format {
  /// Plain text in aligned sections.
  Human,
  Markdown,
  /// A stable JSON summary.
  Json,
}

#[derive(clap::Args)]
pub struct Args {
  /// Run ID prefix, or `latest`.
  #[arg(default_value = "latest")]
  run: String,
  #[arg(long, value_enum, default_value = "summary")]
  level: Level,
  #[arg(short = 'o', long, value_enum, default_value = "human")]
  output: Format,
  /// Write the report to this file instead of stdout.
  #[arg(long)]
  file: Option<PathBuf>,
}

/// Colors only for a terminal, and not when `NO_COLOR` is set
/// (<https://no-color.org>).
fn color(to_file: bool) -> bool {
  !to_file && std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  // NOTICE: the report reads the trace database directly, so it needs the
  // daemon's state directory on this machine.
  let db = ctx.paths()?.state.join("trace.db");
  if !db.exists() {
    anyhow::bail!("no trace database at {}; is NEEDY_STATE_DIR the daemon's?", db.display());
  }
  let loaded = needy_trace::sink::sqlite::load(&db, &args.run).await?;
  let events = loaded
    .events
    .iter()
    .map(|stored| {
      let event = Event::decode(&stored.kind, stored.version, stored.payload.get())
        .with_context(|| format!("run {}: event {} ({}) cannot be read", loaded.run, stored.seq, stored.kind))?;
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
  let format = match args.output {
    Format::Human => report::Format::Human {
      color: color(args.file.is_some()),
    },
    Format::Markdown => report::Format::Markdown,
    Format::Json => report::Format::Json,
  };
  let text = report::render(&loaded.run, &events, level, format);
  match args.file {
    Some(path) => std::fs::write(&path, text).with_context(|| format!("cannot write {}", path.display()))?,
    None => print!("{text}"),
  }
  Ok(0)
}
