//! `needy image log [IMAGE] [--profile P] [--output table|json]`: shows the
//! commit history recorded for an image in the control daemon's layer store.

use std::path::PathBuf;

use anyhow::Result;
use needy_api_client::proto::v1::LogImageRequest;
use serde::Serialize;

use super::lockfile;
use crate::cli::Context;
use crate::output::{self, OutputFormat, Row};

#[derive(clap::Args)]
pub struct Args {
  /// Path to an image definition file (`.yaml`).
  #[arg(value_name = "IMAGE", conflicts_with = "profile")]
  pub image: Option<PathBuf>,
  /// Show the commit history of the image definition used by this profile
  /// (`actor.image`), including `builtin:NAME` profiles.
  #[arg(short = 'f', long, conflicts_with = "image")]
  pub profile: Option<PathBuf>,
  /// Output format.
  #[arg(short = 'o', long, value_enum, default_value_t)]
  pub output: OutputFormat,
}

#[derive(Serialize)]
struct CommandRow {
  argv: Vec<String>,
  cwd: String,
}

#[derive(Serialize)]
struct CommitRow {
  active: bool,
  layer: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  parent: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  base: Option<String>,
  time_ms: i64,
  commands: Vec<CommandRow>,
}

impl Row for CommitRow {
  const HEADER: &'static [&'static str] = &["ACTIVE", "LAYER", "BASE", "COMMANDS"];

  fn cells(&self) -> Vec<String> {
    let active = if self.active { "*" } else { "" }.to_string();
    let base = self.base.as_deref().unwrap_or("-").to_string();
    let commands = if self.commands.is_empty() {
      "-".to_string()
    } else {
      self.commands.iter().map(|c| shell_words::join(&c.argv)).collect::<Vec<_>>().join("; ")
    };
    vec![active, self.layer.clone(), base, commands]
  }
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  let fail = needy_api_client::Error::from;
  let target = lockfile::resolve_target(args.image.as_deref(), args.profile.as_deref(), None, ctx)?;
  let lock = lockfile::read(&target.lockfile)?;
  let current_layer = lock.commit.map(|c| c.layer).unwrap_or_default();

  let progress = output::Progress::new(ctx.connection_feedback(), "Fetching image commit history");
  let connection = ctx.connect().await?;
  let logged = connection
    .images()
    .log_image(LogImageRequest {
      image_name: target.image_name,
      current_layer,
    })
    .await
    .map_err(fail)?
    .into_inner();
  progress.finish();

  let rows: Vec<CommitRow> = logged
    .commits
    .into_iter()
    .map(|entry| CommitRow {
      active: entry.active,
      layer: entry.layer,
      parent: (!entry.parent.is_empty()).then_some(entry.parent),
      base: (!entry.base.is_empty()).then_some(entry.base),
      time_ms: entry.time_ms,
      commands: entry
        .commands
        .into_iter()
        .map(|cmd| CommandRow {
          argv: cmd.argv,
          cwd: cmd.cwd,
        })
        .collect(),
    })
    .collect();

  output::print(args.output, &rows)?;
  Ok(0)
}
