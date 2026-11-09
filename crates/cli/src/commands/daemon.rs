//! `needy daemon`: a control daemon, or an executor daemon on any machine
//! that runs commands.

use std::path::PathBuf;

use anyhow::Result;
use clap::ValueEnum;

use crate::cli::Context;

#[derive(Clone, Copy, ValueEnum)]
enum Mode {
  /// Run processes on this machine only.
  Executor,
  /// Manage sessions, actors and executors.
  Control,
}

#[derive(clap::Args)]
pub struct Args {
  #[arg(long, value_enum, default_value = "executor")]
  mode: Mode,
  /// Where to listen: unix:/path or HOST:PORT. Defaults to the endpoint.
  #[arg(long)]
  listen: Option<String>,
  /// Executor name reported to clients.
  #[arg(long, default_value = "local")]
  name: String,
  /// Environment variable holding the bearer token clients must present.
  #[arg(long)]
  token_env: Option<String>,
  /// Directory with the actor's vmlinux, rootfs.ext4 and firecracker.
  /// Defaults to the image `needy build` writes.
  #[arg(long)]
  actor_image: Option<PathBuf>,
  /// Docker image for docker executors without their own image.
  #[arg(long, default_value = "needy-executor:latest")]
  docker_image: String,
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  let listen = match args.listen {
    Some(listen) => listen,
    None => ctx.endpoint()?,
  };
  let token = args.token_env.as_ref().and_then(|name| std::env::var(name).ok());
  match args.mode {
    Mode::Executor => needy_daemon::executor::serve(&listen, args.name, token).await?,
    #[cfg(not(unix))]
    Mode::Control => anyhow::bail!("the control daemon needs a Unix host"),
    #[cfg(unix)]
    Mode::Control => {
      let paths = ctx.paths()?;
      let image = args.actor_image.unwrap_or_else(|| paths.default_image());
      let daemon = needy_daemon::Daemon::start(needy_daemon::Config {
        state_dir: paths.state,
        runtime_dir: paths.runtime,
        actor_image: image,
        docker_image: args.docker_image,
      })
      .await?;
      let served = daemon.serve(&listen, token).await;
      // Sessions are stopped even when serving failed.
      daemon.shutdown().await;
      served?;
    }
  }
  Ok(0)
}
