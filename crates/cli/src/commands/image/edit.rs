//! `needy image edit [IMAGE] [--profile P]`: opens an interactive writable
//! shell in an image definition, records every program run inside it, commits
//! the upper layer on a clean exit, writes the commit to the image's lockfile,
//! and prints a pasteable `run:` layer snippet.

use std::path::PathBuf;

use anyhow::Result;
use needy_api_client::exec::ExecStream;
use needy_api_client::proto::v1::{ExecAttach, FinishImageEditRequest, StartImageEditRequest};
use needy_config::profile::ActorDriver;
use needy_core_executor::relay;

use super::lockfile::{self, LockCommit};
use crate::cli::Context;
use crate::output::{self, outln};

#[derive(clap::Args)]
pub struct Args {
  /// Path to an image definition file (`.yaml`).
  #[arg(value_name = "IMAGE", conflicts_with = "profile")]
  pub image: Option<PathBuf>,
  /// Edit the image definition used by this profile (`actor.image`).
  #[arg(short = 'f', long, conflicts_with = "image")]
  pub profile: Option<PathBuf>,
  /// Actor driver to run the edit session on (only when editing an image file
  /// directly; defaults to this host's VM driver).
  #[arg(long, value_parser = super::driver)]
  pub driver: Option<ActorDriver>,
  /// Keys that abort the edit session without committing; empty turns detaching off.
  #[arg(long, default_value = relay::DEFAULT_DETACH_KEYS)]
  pub detach_keys: String,
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  let fail = needy_api_client::Error::from;
  let detach_keys = relay::DetachKeys::parse(&args.detach_keys)?;
  let target = lockfile::resolve_target(args.image.as_deref(), args.profile.as_deref(), args.driver, ctx)?;
  let mut lock = lockfile::read(&target.lockfile)?;
  let commit = lock.commit.as_ref().map(LockCommit::to_proto).transpose()?;

  let progress = output::Progress::new(ctx.connection_feedback(), "Starting image edit session");
  let connection = ctx.connect().await?;
  let tty = relay::interactive();
  let window = tty.then(relay::window);
  let started = connection
    .images()
    .start_image_edit(StartImageEditRequest {
      image_name: target.image_name.clone(),
      image_yaml: target.image_yaml,
      driver: target.driver,
      user: target.user,
      commit,
      tty: window,
      wait_for_attach: true,
    })
    .await
    .map_err(fail)?
    .into_inner();
  progress.finish();

  if started.commit_rebased {
    output::message(format_args!("warning: commit layer in {} was applied on a changed base", target.lockfile.display()));
  }

  let attach = ExecAttach {
    run_id: started.session_id.clone(),
    replay: true,
    read_only: false,
    window,
    ..Default::default()
  };
  let stream = ExecStream::attach(&connection, attach).await?;
  let options = relay::Options {
    tty,
    stdin: true,
    read_only: false,
    detach_keys: detach_keys.filter(|_| tty),
  };
  let ended = relay::run(stream, options).await;
  let (abort, exit_code) = match &ended {
    Ok(relay::Ended::Exited(exit)) => {
      let code = exit.code.unwrap_or(1);
      (false, code)
    }
    Ok(relay::Ended::Detached { .. }) => {
      output::after_terminal("detached from image edit session; changes were not committed");
      (true, 1)
    }
    Err(_) => (true, 1),
  };

  let progress = output::Progress::new(
    ctx.connection_feedback(),
    if abort {
      "Aborting edit session"
    } else {
      "Committing image changes"
    },
  );
  let finished = connection
    .images()
    .finish_image_edit(FinishImageEditRequest {
      session_id: started.session_id,
      abort,
    })
    .await
    .map_err(fail)?
    .into_inner();
  progress.finish();

  if let Err(err) = ended {
    return Err(err.into());
  }

  if abort {
    return Ok(exit_code);
  }

  match finished.commit {
    Some(new_commit) => {
      let entry = LockCommit::from_proto(&new_commit)?;
      lock.commit = Some(entry);
      lockfile::write(&target.lockfile, &lock)?;
      output::message(format_args!("image edit: committed {} (lockfile {})", new_commit.layer, target.lockfile.display()));
      if let Some(snippet) = lockfile::format_run_snippet(&finished.commands) {
        output::message("Recorded commands as a `run:` layer:");
        outln!("{snippet}")?;
      }
      Ok(0)
    }
    None => {
      output::message("image edit: session did not exit cleanly; no commit was made");
      let code = finished.exit.and_then(|e| e.code).unwrap_or(exit_code);
      Ok(if code == 0 { 1 } else { code })
    }
  }
}
