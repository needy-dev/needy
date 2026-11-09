//! `needy fs cp`: copies files between this machine and an executor by
//! streaming a tar archive through an exec stream. Every executor platform has
//! `tar`, including Windows.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use clap::Subcommand;
use needy_api_proto::v1::{ExecRequest, ExecStart, exec_request, exec_response};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::cli::connect;

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
    /// Look the executor up in this session.
    #[arg(long, default_value = "", hide_default_value = true)]
    session: String,
  },
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  match args.command {
    Command::Cp {
      source,
      destination,
      session,
    } => cp(&ctx.endpoint()?, &session, &source, &destination).await?,
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
async fn cp(endpoint: &str, session: &str, source: &str, destination: &str) -> Result<()> {
  match (parse(source), parse(destination)) {
    (Location::Local(local), Location::Remote { executor, path }) => upload(endpoint, session, &executor, &local, &path).await,
    (Location::Remote { executor, path }, Location::Local(local)) => download(endpoint, session, &executor, &path, &local).await,
    _ => bail!("one side must be EXECUTOR:path and the other a local path"),
  }
}

/// Runs `tar` on the executor, feeding `input` to its stdin, and returns its stdout.
async fn run_tar(endpoint: &str, session: &str, executor: &str, argv: Vec<String>, input: Option<Vec<u8>>) -> Result<Vec<u8>> {
  let client = connect(endpoint).await?;
  let (tx, rx) = mpsc::channel::<ExecRequest>(64);
  tx.send(ExecRequest {
    payload: Some(exec_request::Payload::Start(ExecStart {
      argv,
      executor: executor.to_string(),
      session_id: session.to_string(),
      no_stdin: input.is_none(),
      origin: needy_api_proto::v1::Origin::Fs.into(),
      // macOS tar would otherwise add AppleDouble `._` files.
      env: [("COPYFILE_DISABLE".to_string(), "1".to_string())].into(),
      ..Default::default()
    })),
  })
  .await?;
  if let Some(bytes) = input {
    let tx = tx.clone();
    tokio::spawn(async move {
      for chunk in bytes.chunks(1 << 20) {
        if tx
          .send(ExecRequest {
            payload: Some(exec_request::Payload::Stdin(chunk.to_vec())),
          })
          .await
          .is_err()
        {
          return;
        }
      }
      // A closed channel means tar already exited; its status reports why.
      if tx
        .send(ExecRequest {
          payload: Some(exec_request::Payload::StdinEof(true)),
        })
        .await
        .is_err()
      {
        tracing::debug!("tar on the executor exited before end of input");
      }
    });
  }
  let mut responses = client.processes().exec(ReceiverStream::new(rx)).await.map_err(|s| anyhow::anyhow!("{}", s.message()))?.into_inner();
  let mut out = Vec::new();
  let mut err = Vec::new();
  while let Some(message) = responses.message().await.map_err(|s| anyhow::anyhow!("{}", s.message()))? {
    match message.payload {
      Some(exec_response::Payload::Stdout(b)) => out.extend_from_slice(&b),
      Some(exec_response::Payload::Stderr(b)) => err.extend_from_slice(&b),
      Some(exec_response::Payload::Exited(exit)) => {
        if exit.code != Some(0) {
          bail!("tar on {executor} failed: {}", String::from_utf8_lossy(&err).trim());
        }
        break;
      }
      Some(exec_response::Payload::Error(e)) => bail!("{e}"),
      _ => {}
    }
  }
  drop(tx);
  Ok(out)
}

async fn upload(endpoint: &str, session: &str, executor: &str, local: &Path, remote_dir: &str) -> Result<()> {
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
  run_tar(
    endpoint,
    session,
    executor,
    vec![
      "tar".into(),
      "-x".into(),
      "-f".into(),
      "-".into(),
      "-C".into(),
      remote_dir.into(),
    ],
    Some(archive),
  )
  .await?;
  tracing::info!("copied {size} bytes to {executor}:{remote_dir}");
  Ok(())
}

async fn download(endpoint: &str, session: &str, executor: &str, remote: &str, local_dir: &Path) -> Result<()> {
  let (parent, name) = split(remote);
  let archive = run_tar(
    endpoint,
    session,
    executor,
    vec![
      "tar".into(),
      "-c".into(),
      "-f".into(),
      "-".into(),
      "-C".into(),
      parent,
      name,
    ],
    None,
  )
  .await?;
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
