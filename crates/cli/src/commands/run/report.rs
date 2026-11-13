//! `needy run report`: a report of a run from the trace, as plain text for
//! people, markdown or JSON. The control plane sends the run's events
//! (`RunService.GetRunEvents`), so any client it reaches can report; with
//! `--offline` they are read from this machine's `trace.db` instead, offline.
//! Either way they are decoded into `needy_core::event` values and rendered
//! by `needy_core::report`; this command picks the format and the colors.

use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use clap::ValueEnum;
use needy_api_client::proto::v1::GetRunEventsRequest;
use needy_core::event::{Event, Recorded};
use needy_core::report;

use crate::cli::Context;
use crate::output::out;

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
  /// Run ID, unique prefix, name (online only), or latest. Defaults to the inherited run, else latest.
  run: Option<String>,
  #[arg(long, value_enum, default_value = "summary")]
  level: Level,
  #[arg(short = 'o', long, value_enum, default_value = "human")]
  output: Format,
  /// Write the report to this file instead of stdout.
  #[arg(long)]
  file: Option<PathBuf>,
  /// Read the trace database in this machine's state directory
  /// (`NEEDY_STATE_DIR`) instead of asking the control plane, as on the
  /// control host with no daemon running.
  #[arg(long)]
  offline: bool,
}

/// Colors only for a terminal, and not when `NO_COLOR` is set
/// (<https://no-color.org>).
fn color(to_file: bool) -> bool {
  !to_file && std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
}

/// One stored event, from either source.
struct Stored {
  seq: u64,
  time_ms: i64,
  kind: String,
  version: u32,
  payload: String,
}

/// The run's full ID and its events, from this machine's `trace.db`.
async fn local_events(ctx: &Context, run: &str) -> Result<(String, Vec<Stored>)> {
  let db = ctx.paths()?.state.join("trace.db");
  if !db.exists() {
    anyhow::bail!("no trace database at {}; is NEEDY_STATE_DIR the daemon's?", db.display());
  }
  let loaded = needy_trace::sink::sqlite::load(&db, run).await?;
  let events = loaded
    .events
    .into_iter()
    .map(|event| Stored {
      seq: event.seq,
      time_ms: event.time_ms,
      kind: event.kind,
      version: event.version,
      payload: event.payload.get().to_string(),
    })
    .collect();
  Ok((loaded.run, events))
}

/// The run's full ID and its events, from the control plane.
async fn remote_events(ctx: &Context, run: &str) -> Result<(String, Vec<Stored>)> {
  let fail = needy_api_client::Error::from;
  let connection = ctx.connect().await?;
  let selected = crate::runs::select(&connection, Some(run.to_string()), ctx).await?;
  let request = GetRunEventsRequest { run_id: selected };
  let mut stream = connection.runs().get_run_events(request).await.map_err(fail)?.into_inner();
  let mut id = String::new();
  let mut events = Vec::new();
  while let Some(message) = stream.message().await.map_err(fail)? {
    id = message.run_id;
    events.extend(message.events.into_iter().map(|event| Stored {
      seq: event.seq,
      time_ms: event.time_ms,
      kind: event.kind,
      version: event.version,
      payload: event.payload,
    }));
  }
  Ok((id, events))
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  // The control plane and `trace.db` read a prefix and `latest` themselves.
  let wanted = crate::runs::wanted(args.run, ctx)?.unwrap_or_else(|| "latest".into());
  let (run, stored) = if args.offline {
    local_events(ctx, &wanted).await?
  } else {
    remote_events(ctx, &wanted).await?
  };
  let events = stored
    .iter()
    .map(|stored| {
      let event = Event::decode(&stored.kind, stored.version, &stored.payload)
        .with_context(|| format!("run {run}: event {} ({}) cannot be read", stored.seq, stored.kind))?;
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
  let text = report::render(&run, &events, level, format);
  match args.file {
    Some(path) => std::fs::write(&path, text).with_context(|| format!("cannot write {}", path.display()))?,
    None => out!("{text}")?,
  }
  Ok(0)
}
