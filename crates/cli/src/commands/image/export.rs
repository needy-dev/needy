//! `needy image export [IMAGE] [--profile P] --output <PATH>`: builds or
//! resolves an image definition on the control daemon and writes its
//! Docker/OCI tar archive to `<PATH>` (atomically) or stdout (`-`).

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use needy_api_client::proto::v1::ExportImageRequest;
use needy_config::profile::ActorDriver;

use super::lockfile::{self, LockCommit};
use crate::cli::Context;
use crate::output;

#[derive(clap::Args)]
pub struct Args {
  /// Path to an image definition file (`.yaml`).
  #[arg(value_name = "IMAGE", conflicts_with = "profile")]
  pub image: Option<PathBuf>,
  /// Export the image definition used by this profile (`actor.image`),
  /// including `builtin:NAME` profiles.
  #[arg(short = 'f', long, conflicts_with = "image")]
  pub profile: Option<PathBuf>,
  /// Actor driver to export the image for (defaults to this host's VM driver
  /// when exporting an image file directly).
  #[arg(long, value_parser = super::driver)]
  pub driver: Option<ActorDriver>,
  /// Destination tar archive path, or `-` to write to stdout.
  #[arg(short = 'o', long, value_name = "PATH")]
  pub output: PathBuf,
  /// Optional `repository:tag` embedded in the exported archive's manifest.
  #[arg(long)]
  pub tag: Option<String>,
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  let fail = needy_api_client::Error::from;
  let target = lockfile::resolve_target(args.image.as_deref(), args.profile.as_deref(), args.driver, ctx)?;
  let lock = lockfile::read(&target.lockfile)?;
  let commit = lock.commit.as_ref().map(LockCommit::to_proto).transpose()?;

  let progress = output::Progress::new(ctx.connection_feedback(), "Exporting image archive");
  let connection = ctx.connect().await?;
  let mut stream = connection
    .images()
    .export_image(ExportImageRequest {
      image_yaml: target.image_yaml,
      driver: target.driver,
      user: target.user,
      commit,
      pinned_features: lock.features.into_iter().collect(),
      tag: args.tag.unwrap_or_default(),
    })
    .await
    .map_err(fail)?
    .into_inner();

  if args.output == Path::new("-") {
    progress.finish();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    while let Some(chunk) = stream.message().await.map_err(fail)? {
      out.write_all(&chunk.data).context("cannot write exported archive to stdout")?;
    }
    out.flush().context("cannot flush stdout")?;
    return Ok(0);
  }

  let dir = args.output.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
  std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
  let staged = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));

  let write_result = async {
    let mut file = std::fs::OpenOptions::new()
      .write(true)
      .create_new(true)
      .open(&staged)
      .with_context(|| format!("cannot stage {}", args.output.display()))?;
    let mut total: u64 = 0;
    while let Some(chunk) = stream.message().await.map_err(fail)? {
      file.write_all(&chunk.data).with_context(|| format!("cannot write {}", args.output.display()))?;
      total += chunk.data.len() as u64;
    }
    file.sync_all().with_context(|| format!("cannot sync {}", args.output.display()))?;
    Ok::<u64, anyhow::Error>(total)
  }
  .await;

  progress.finish();
  match write_result {
    Ok(bytes) => {
      std::fs::rename(&staged, &args.output).with_context(|| format!("cannot persist {}", args.output.display()))?;
      output::message(format_args!("image export: wrote {} ({bytes} bytes)", args.output.display()));
      Ok(0)
    }
    Err(err) => {
      // Remove the partial staged archive on failure; cleanup errors are secondary.
      let _ = std::fs::remove_file(&staged);
      Err(err)
    }
  }
}
