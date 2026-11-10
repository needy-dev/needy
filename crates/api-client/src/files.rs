//! Files to and from an executor as tar archives over exec streams. Every
//! executor platform has `tar`, including Windows. Packing and unpacking on
//! this side is the caller's (`needy_common::archive`), since copy-in
//! filters and `needy fs cp` name what goes into an archive differently.

use needy_api_proto::v1::{ExecStart, Origin};

use crate::exec::ExecStream;
use crate::{Client, Error, Kind, Result};

/// Where the executor's `tar` runs, and who asks.
pub struct Target {
  /// The executor's name within the run; empty on an executor daemon.
  pub executor: String,
  /// Empty outside a run.
  pub run: String,
  pub origin: Origin,
}

/// Unpacks `archive` into the directory `dir` on the executor, creating
/// `dir` and its missing parents first.
pub async fn push(client: &Client, target: &Target, archive: Vec<u8>, dir: &str) -> Result<()> {
  make_dir(client, target, dir).await?;
  let argv = ["tar", "-x", "-f", "-", "-C", dir];
  run(client, target, &argv, Some(archive)).await?;
  Ok(())
}

/// Creates the directory `dir` and its missing parents on the executor;
/// an existing directory is fine.
///
/// NOTICE: `tar -C` does not create its directory, and neither GNU tar nor
/// bsdtar has a portable option that does. The executor's platform is not
/// known here (through a run the client talks to the control daemon), so
/// `mkdir -p` runs first, and where no `mkdir` program can start, as on
/// Windows, `cmd`'s built-in `mkdir`, which creates parents too.
async fn make_dir(client: &Client, target: &Target, dir: &str) -> Result<()> {
  match run(client, target, &["mkdir", "-p", "--", dir], None).await {
    Err(e @ Error::Status(_)) if e.kind() == Kind::Failed => {
      tracing::debug!("no mkdir program on the executor ({e}); trying cmd");
      // `if not exist` keeps an existing directory from being an error;
      // `cmd` reads `/` as the start of a switch.
      let dir = dir.replace('/', "\\");
      run(client, target, &["cmd", "/d", "/c", "if", "not", "exist", &dir, "mkdir", &dir], None).await.map(drop)
    }
    made => made.map(drop),
  }
}

/// Packs `path`, relative to the directory `dir` on the executor; `.` packs
/// the directory's contents.
pub async fn pull(client: &Client, target: &Target, dir: &str, path: &str) -> Result<Vec<u8>> {
  let argv = ["tar", "-c", "-f", "-", "-C", dir, path];
  run(client, target, &argv, None).await
}

/// Runs `argv` (`tar`, or what prepares for it) on the executor and returns
/// its standard output.
async fn run(client: &Client, target: &Target, argv: &[&str], stdin: Option<Vec<u8>>) -> Result<Vec<u8>> {
  let start = ExecStart {
    argv: argv.iter().map(|a| a.to_string()).collect(),
    executor: target.executor.clone(),
    run_id: target.run.clone(),
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
