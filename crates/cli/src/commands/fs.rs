//! `needy fs cp`: copies files between this machine and an executor as tar
//! archives over exec streams (`needy_api_client::files`).

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use clap::Subcommand;
use needy_api_client::files::{self, Target};
use needy_api_client::proto::v1::Origin;

use crate::cli::{Context, connect};

#[derive(clap::Args)]
pub struct Args {
  #[command(subcommand)]
  command: Command,
}

#[derive(Subcommand)]
enum Command {
  /// Copy a file or directory into a directory: `SRC EXECUTOR:DIR` or `EXECUTOR:PATH DIR`.
  Cp {
    source: String,
    destination: String,
    /// Look the executor up in this run.
    #[arg(long, default_value = "", hide_default_value = true)]
    run: String,
  },
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  match args.command {
    Command::Cp {
      source,
      destination,
      run,
    } => cp(&ctx.endpoint()?, &run, &source, &destination).await?,
  }
  Ok(0)
}

/// `EXECUTOR:path`, or a local path.
enum Location {
  Local(PathBuf),
  Remote { executor: String, path: String },
}

fn parse(spec: &str) -> Location {
  // A Windows drive letter is a local path, not an executor name.
  match spec.split_once(':') {
    Some((executor, path)) if executor.len() > 1 && !executor.contains('/') => Location::Remote {
      executor: executor.to_string(),
      path: path.to_string(),
    },
    _ => Location::Local(PathBuf::from(spec)),
  }
}

fn split(path: &str) -> (String, String) {
  let trimmed = path.trim_end_matches(['/', '\\']);
  match trimmed.rfind(['/', '\\']) {
    Some(i) => (
      if i == 0 {
        "/".into()
      } else {
        trimmed[..i].to_string()
      },
      trimmed[i + 1..].to_string(),
    ),
    None => (".".into(), trimmed.to_string()),
  }
}

/// Copies `source` into the directory `destination`, keeping its name.
async fn cp(endpoint: &str, run: &str, source: &str, destination: &str) -> Result<()> {
  match (parse(source), parse(destination)) {
    (Location::Local(local), Location::Remote { executor, path }) => upload(endpoint, run, &executor, &local, &path).await,
    (Location::Remote { executor, path }, Location::Local(local)) => download(endpoint, run, &executor, &path, &local).await,
    _ => bail!("one side must be EXECUTOR:path and the other a local path"),
  }
}

/// A failed `tar` names the executor; the daemon's refusals speak for
/// themselves.
fn tar_failed(executor: &str, error: needy_api_client::Error) -> anyhow::Error {
  match error {
    needy_api_client::Error::Failed { .. } => anyhow::anyhow!("tar on {executor} failed: {error}"),
    error => error.into(),
  }
}

fn target(run: &str, executor: &str) -> Target {
  Target {
    executor: executor.to_string(),
    run: run.to_string(),
    runner: String::new(),
    origin: Origin::Fs,
  }
}

async fn upload(endpoint: &str, run: &str, executor: &str, local: &Path, remote_dir: &str) -> Result<()> {
  let name = local.file_name().context("source has no file name")?.to_os_string();
  let local = local.to_path_buf();
  let archive = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());
    builder.follow_symlinks(false);
    if local.is_dir() {
      builder.append_dir_all(&name, &local)?;
    } else {
      builder.append_path_with_name(&local, &name)?;
    }
    Ok(builder.into_inner()?)
  })
  .await??;
  let size = archive.len();
  let client = connect(endpoint).await?;
  files::push(&client, &target(run, executor), archive, remote_dir).await.map_err(|e| tar_failed(executor, e))?;
  tracing::info!("copied {size} bytes to {executor}:{remote_dir}");
  Ok(())
}

async fn download(endpoint: &str, run: &str, executor: &str, remote: &str, local_dir: &Path) -> Result<()> {
  let (parent, name) = split(remote);
  let client = connect(endpoint).await?;
  let archive = files::pull(&client, &target(run, executor), &parent, &name).await.map_err(|e| tar_failed(executor, e))?;
  let size = archive.len();
  let local_dir = local_dir.to_path_buf();
  tokio::task::spawn_blocking(move || -> Result<()> {
    std::fs::create_dir_all(&local_dir)?;
    let mut tar = tar::Archive::new(std::io::Cursor::new(archive));
    for entry in tar.entries()? {
      let mut entry = entry?;
      // Refuse paths that escape the destination.
      let path = entry.path()?.into_owned();
      if path.is_absolute() || path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        bail!("unsafe path in archive: {}", path.display());
      }
      entry.unpack_in(&local_dir)?;
    }
    Ok(())
  })
  .await??;
  tracing::info!("copied {size} bytes from {executor}:{remote}");
  Ok(())
}
