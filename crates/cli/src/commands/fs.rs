//! `needy fs cp`: copies files between this machine and an executor as tar
//! archives over exec streams (`needy_api_client::files`).

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use clap::Subcommand;
use needy_api_client::Client;
use needy_api_client::files::{self, Target};
use needy_api_client::proto::v1::Origin;

use crate::cli::Context;

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
    /// Bound run; requires its named executor.
    #[arg(long, requires = "executor", conflicts_with_all = ["runner", "host"])]
    run: Option<String>,
    /// One registered runner's machine.
    #[arg(long, conflicts_with_all = ["executor", "host"])]
    runner: Option<String>,
    /// The selected control or standalone executor host.
    #[arg(long, conflicts_with = "executor")]
    host: bool,
    /// A run-bound executor, or a uniquely named pooled executor outside a run.
    #[arg(long)]
    executor: Option<String>,
  },
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  match args.command {
    Command::Cp {
      source,
      destination,
      run,
      runner,
      host,
      executor,
    } => {
      let count = usize::from(runner.is_some()) + usize::from(host) + usize::from(executor.is_some());
      if count != 1
        || !matches!(
          (parse(&source), parse(&destination)),
          (Location::Local(_), Location::Remote { .. }) | (Location::Remote { .. }, Location::Local(_))
        )
      {
        return Err(
          crate::diagnostics::Error::new(
            "files.invalid_target",
            "Copy requires one local path and one remote:PATH with one explicit target",
          )
          .check("Use --host, --runner NAME, --executor NAME, or --run RUN --executor NAME.")
          .into(),
        );
      }
      let client = ctx.connect().await?;
      let place = if let Some(runner) = runner {
        Place::Runner(runner)
      } else if let Some(executor) = executor {
        Place::Run {
          run: match run {
            Some(run) => crate::runs::select(&client, Some(run), ctx).await?,
            None => String::new(),
          },
          executor,
        }
      } else {
        Place::Host
      };
      cp(&client, &place, &source, &destination).await?
    }
  }
  Ok(0)
}

/// What the `EXECUTOR:` side of a copy names.
enum Place {
  Run { run: String, executor: String },
  Runner(String),
  Host,
}

enum Location {
  Local(PathBuf),
  Remote { path: String },
}

fn parse(spec: &str) -> Location {
  // Only the reserved marker is remote. Windows drive letters, filenames
  // containing colons and platform path syntax stay local.
  match spec.strip_prefix("remote:") {
    Some(path) => Location::Remote {
      path: path.to_string(),
    },
    None => Location::Local(PathBuf::from(spec)),
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
async fn cp(client: &Client, place: &Place, source: &str, destination: &str) -> Result<()> {
  match (parse(source), parse(destination)) {
    (Location::Local(local), Location::Remote { path }) => upload(client, place, &place_name(place), &local, &path).await,
    (Location::Remote { path }, Location::Local(local)) => download(client, place, &place_name(place), &path, &local).await,
    _ => bail!("one side must be remote:PATH and the other a local path"),
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

fn place_name(place: &Place) -> String {
  match place {
    Place::Run { executor, .. } => executor.clone(),
    Place::Runner(name) => name.clone(),
    Place::Host => "host".into(),
  }
}

fn target(place: &Place, _: &str) -> Target {
  let (executor, run, runner) = match place {
    Place::Run { run, executor } => (executor.clone(), run.clone(), String::new()),
    Place::Runner(name) => (String::new(), String::new(), name.clone()),
    Place::Host => (String::new(), String::new(), String::new()),
  };
  Target {
    executor,
    run,
    runner,
    origin: Origin::Fs,
  }
}

async fn upload(client: &Client, place: &Place, executor: &str, local: &Path, remote_dir: &str) -> Result<()> {
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
  files::push(client, &target(place, executor), archive, remote_dir).await.map_err(|e| tar_failed(executor, remote_dir, e))?;
  tracing::info!("copied {size} bytes to {executor}:{remote_dir}");
  Ok(())
}

async fn download(client: &Client, place: &Place, executor: &str, remote: &str, local_dir: &Path) -> Result<()> {
  let (parent, name) = split(remote);
  let archive = files::pull(client, &target(place, executor), &parent, &name).await.map_err(|e| tar_failed(executor, remote, e))?;
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
        return Err(crate::diagnostics::Error::new("files.unsafe_archive", "The archive contains a path outside the destination").into());
      }
      if !entry.unpack_in(&local_dir)? {
        return Err(
          crate::diagnostics::Error::new("files.unsafe_archive", "The archive entry could not be extracted inside the destination").into(),
        );
      }
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
