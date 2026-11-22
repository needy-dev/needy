//! `needy image rollback [IMAGE] [--profile P] [@N]`: rolls an image's commit
//! pointer back by `N` commits (default 1) and updates its lockfile.

use std::path::PathBuf;

use anyhow::{Result, bail};
use needy_api_client::proto::v1::RollbackImageRequest;

use super::lockfile;
use crate::cli::{ConnectionArgs, Context};
use crate::output;

#[derive(clap::Args)]
pub struct Args {
  /// Roll back the image definition used by this profile (`actor.image`).
  #[arg(short = 'f', long)]
  pub profile: Option<PathBuf>,
  /// Image file path and/or `@N` steps to roll back (default `@1`).
  #[arg(value_name = "IMAGE|@N")]
  pub args: Vec<String>,
  #[command(flatten)]
  pub(crate) connection: ConnectionArgs,
}

/// Parses `[IMAGE]` and optional `@N` step count from `args`, enforcing that
/// either `IMAGE` or `--profile` is given (not both).
pub(crate) fn parse_args(profile: Option<PathBuf>, args: &[String]) -> Result<(Option<PathBuf>, Option<PathBuf>, u32)> {
  let mut image: Option<PathBuf> = None;
  let mut steps: Option<u32> = None;
  for arg in args {
    if let Some(rest) = arg.strip_prefix('@') {
      if steps.is_some() {
        bail!("rollback steps (`@N`) may only be given once");
      }
      let n: u32 =
        rest.parse().map_err(|_| anyhow::anyhow!("{arg:?} is not a valid rollback step count (expected `@N`, e.g. `@1` or `@2`)"))?;
      if n == 0 {
        bail!("rollback steps must be at least 1 (`@1`)");
      }
      steps = Some(n);
    } else {
      if image.is_some() {
        bail!("unexpected extra argument {arg:?}");
      }
      image = Some(PathBuf::from(arg));
    }
  }
  if image.is_some() && profile.is_some() {
    bail!("name an image file or `--profile`, not both");
  }
  if image.is_none() && profile.is_none() {
    bail!("name an image file or pass `--profile PROFILE`");
  }
  Ok((image, profile, steps.unwrap_or(1)))
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  let fail = needy_api_client::Error::from;
  let (image, profile, steps) = parse_args(args.profile, &args.args)?;
  let target = lockfile::resolve_target(image.as_deref(), profile.as_deref(), None, ctx)?;
  let mut lock = lockfile::read(&target.lockfile)?;
  let Some(current) = lock.commit.clone() else {
    bail!("{} has no commits in lockfile {}", target.image_name, target.lockfile.display());
  };

  let progress = output::Progress::new(ctx.connection_feedback(), "Rolling back image commit");
  let connection = ctx.connect().await?;
  let rolled = connection
    .images()
    .rollback_image(RollbackImageRequest {
      image_name: target.image_name,
      from_layer: current.layer,
      steps,
    })
    .await
    .map_err(fail)?
    .into_inner();
  progress.finish();

  if rolled.layer.is_empty() {
    lock.commit = None;
    lockfile::write(&target.lockfile, &lock)?;
    output::acknowledge(&format!("Cleared commit layer in {} (back to built layers).", target.lockfile.display()))?;
  } else {
    let base = if rolled.base.is_empty() {
      current.base
    } else {
      rolled.base
    };
    lock.commit = Some(lockfile::LockCommit {
      layer: rolled.layer.clone(),
      base,
    });
    lockfile::write(&target.lockfile, &lock)?;
    output::acknowledge(&format!("Rolled back {} -> {} (lockfile {}).", steps, rolled.layer, target.lockfile.display()))?;
  }
  Ok(0)
}
