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
    /// `EXECUTOR:` names a runner instead: copy to or from its machine.
    #[arg(long, conflicts_with = "run")]
    runner: bool,
  },
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  match args.command {
    Command::Cp {
      source,
      destination,
      run,
      runner,
    } => {
      let place = if runner {
        Place::Runner
      } else {
        Place::Run(run)
      };
      cp(&ctx.endpoint()?, &place, &source, &destination).await?
    }
  }
  Ok(0)
}

/// What the `EXECUTOR:` side of a copy names.
enum Place {
  /// An executor, within this run when it is not empty.
  Run(String),
  /// A runner's machine.
  Runner,
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
async fn cp(endpoint: &str, place: &Place, source: &str, destination: &str) -> Result<()> {
  match (parse(source), parse(destination)) {
    (Location::Local(local), Location::Remote { executor, path }) => upload(endpoint, place, &executor, &local, &path).await,
    (Location::Remote { executor, path }, Location::Local(local)) => download(endpoint, place, &executor, &path, &local).await,
    _ => bail!("one side must be EXECUTOR:path and the other a local path"),
  }
}

/// A failed `tar` (or the `mkdir` before it) names the executor and the path
/// of the copy, since tar's own message may not: Windows' bsdtar says
/// `tar: : Couldn't visit directory` for a missing source. The daemon's
/// refusals speak for themselves.
fn tar_failed(executor: &str, path: &str, error: needy_api_client::Error) -> anyhow::Error {
  match error {
    needy_api_client::Error::Failed { .. } => anyhow::anyhow!("cannot copy {executor}:{path}: {error}"),
    error => error.into(),
  }
}

fn target(place: &Place, name: &str) -> Target {
  let (executor, run, runner) = match place {
    Place::Run(run) => (name.to_string(), run.clone(), String::new()),
    Place::Runner => (String::new(), String::new(), name.to_string()),
  };
  Target {
    executor,
    run,
    runner,
    origin: Origin::Fs,
  }
}

async fn upload(endpoint: &str, place: &Place, executor: &str, local: &Path, remote_dir: &str) -> Result<()> {
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
  files::push(&client, &target(place, executor), archive, remote_dir).await.map_err(|e| tar_failed(executor, remote_dir, e))?;
  tracing::info!("copied {size} bytes to {executor}:{remote_dir}");
  Ok(())
}

async fn download(endpoint: &str, place: &Place, executor: &str, remote: &str, local_dir: &Path) -> Result<()> {
  let (parent, name) = split(remote);
  let client = connect(endpoint).await?;
  let archive = files::pull(&client, &target(place, executor), &parent, &name).await.map_err(|e| tar_failed(executor, remote, e))?;
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

#[cfg(test)]
#[path = "fs_test.rs"]
mod tests;
