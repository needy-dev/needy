//! `needy image prune [--all] [--dry-run] [--output table|json]`: garbage-
//! collects unreferenced cached layers, disks, OCI archives, stale cache keys,
//! and `needy-snapshot:*` container images on the control daemon.

use anyhow::Result;
use needy_api_client::proto::v1::PruneImagesRequest;
use serde::Serialize;

use crate::cli::{ConnectionArgs, Context};
use crate::output::{self, OutputFormat, outln};

#[derive(clap::Args)]
pub struct Args {
  /// Also prune cached base container image exports (`base-oci/*.tar`).
  #[arg(short = 'a', long)]
  pub all: bool,
  /// Show what would be removed without deleting anything.
  #[arg(short = 'n', long)]
  pub dry_run: bool,
  /// Output format.
  #[arg(short = 'o', long, value_enum, default_value_t)]
  pub output: OutputFormat,
  #[command(flatten)]
  pub(crate) connection: ConnectionArgs,
}

#[derive(Serialize)]
struct PruneReport {
  dry_run: bool,
  layers_removed: u64,
  snapshots_removed: u64,
  bytes_reclaimed: u64,
  removed_images: Vec<String>,
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  let fail = needy_api_client::Error::from;
  let progress = output::Progress::new(
    ctx.connection_progress(matches!(args.output, OutputFormat::Table)),
    if args.dry_run {
      "Checking prunable images"
    } else {
      "Pruning unused images"
    },
  );
  let connection = ctx.connect().await?;
  let pruned = connection
    .images()
    .prune_images(PruneImagesRequest {
      all: args.all,
      dry_run: args.dry_run,
      keep: Vec::new(),
    })
    .await
    .map_err(fail)?
    .into_inner();
  progress.finish();

  let report = PruneReport {
    dry_run: args.dry_run,
    layers_removed: pruned.layers_removed,
    snapshots_removed: pruned.snapshots_removed,
    bytes_reclaimed: pruned.bytes_reclaimed,
    removed_images: pruned.removed_images,
  };

  match args.output {
    OutputFormat::Json => {
      outln!("{}", serde_json::to_string_pretty(&report)?)?;
    }
    OutputFormat::Table => {
      let verb = if report.dry_run {
        "Would reclaim"
      } else {
        "Reclaimed"
      };
      outln!(
        "{verb} {} ({} {}, {} container {})",
        output::bytes(report.bytes_reclaimed),
        report.layers_removed,
        if report.layers_removed == 1 {
          "layer"
        } else {
          "layers"
        },
        report.snapshots_removed,
        if report.snapshots_removed == 1 {
          "snapshot"
        } else {
          "snapshots"
        },
      )?;
      for image in &report.removed_images {
        outln!("  {image}")?;
      }
    }
  }
  Ok(0)
}
