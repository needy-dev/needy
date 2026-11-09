//! Files to and from an executor as tar archives over exec streams. Every
//! executor platform has `tar`, including Windows. Packing and unpacking on
//! this side is the caller's (`needy_common::archive`), since copy-in
//! filters and `needy fs cp` name what goes into an archive differently.

use needy_api_proto::v1::{ExecStart, Origin};

use crate::exec::ExecStream;
use crate::{Client, Error, Result};

/// Where the executor's `tar` runs, and who asks.
pub struct Target {
  /// The executor's name within the session; empty on an executor daemon.
  pub executor: String,
  /// Empty outside a session.
  pub session: String,
  pub origin: Origin,
}

/// Unpacks `archive` into the directory `dir` on the executor.
// TODO(8g): `tar -C` fails when `dir` does not exist; fix slice 8g creates
// it, with a regression test.
pub async fn push(client: &Client, target: &Target, archive: Vec<u8>, dir: &str) -> Result<()> {
  let argv = ["tar", "-x", "-f", "-", "-C", dir];
  tar(client, target, &argv, Some(archive)).await?;
  Ok(())
}

/// Packs `path`, relative to the directory `dir` on the executor; `.` packs
/// the directory's contents.
pub async fn pull(client: &Client, target: &Target, dir: &str, path: &str) -> Result<Vec<u8>> {
  let argv = ["tar", "-c", "-f", "-", "-C", dir, path];
  tar(client, target, &argv, None).await
}

/// Runs `tar` on the executor and returns its standard output.
async fn tar(client: &Client, target: &Target, argv: &[&str], stdin: Option<Vec<u8>>) -> Result<Vec<u8>> {
  let start = ExecStart {
    argv: argv.iter().map(|a| a.to_string()).collect(),
    executor: target.executor.clone(),
    session_id: target.session.clone(),
    no_stdin: stdin.is_none(),
    origin: target.origin.into(),
    // macOS tar would otherwise add AppleDouble `._` files.
    env: [("COPYFILE_DISABLE".to_string(), "1".to_string())].into(),
    ..Default::default()
  };
  let output = ExecStream::start(client, start).await?.run(stdin).await?;
  if output.exit.code != Some(0) {
    return Err(Error::Failed {
      exit: output.exit,
      stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    });
  }
  Ok(output.stdout)
}
