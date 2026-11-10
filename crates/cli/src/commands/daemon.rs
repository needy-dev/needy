//! `needy daemon`: a control daemon, an executor daemon on any machine
//! that runs commands, or a runner: a machine that joined a control plane
//! and dials it for the commands to run.

use std::path::PathBuf;

use anyhow::{Context as _, Result, anyhow, bail};
use clap::ValueEnum;

use crate::cli::Context;

#[derive(Clone, Copy, ValueEnum)]
enum Mode {
  /// Run processes on this machine only.
  Executor,
  /// Manage runs, actors and executors, and the runners that joined.
  Control,
  /// Join a control plane as a runner, keep a connection to it, and run
  /// the processes it asks for on this machine. Listens nowhere.
  Runner,
}

#[derive(clap::Args)]
pub struct Args {
  #[arg(long, value_enum, default_value = "executor")]
  mode: Mode,
  /// Where to listen: unix:/path or HOST:PORT. Defaults to the endpoint.
  /// Not in runner mode, which dials the control plane instead.
  #[arg(long)]
  listen: Option<String>,
  /// Executor name reported to clients: `local` by default. A runner's
  /// name, by default this machine's host name up to its first dot.
  #[arg(long)]
  name: Option<String>,
  /// Environment variable holding the bearer token clients must present.
  /// Required to listen on a TCP address other than loopback.
  #[arg(long)]
  token_env: Option<String>,
  /// Directory with the actor's vmlinux, rootfs.ext4 and firecracker.
  /// Defaults to the image `needy build` writes.
  #[arg(long)]
  actor_image: Option<PathBuf>,
  /// Docker image for docker executors without their own image.
  #[arg(long, default_value = "needy-executor:latest")]
  docker_image: String,
  /// How often runners send a heartbeat (control mode). A runner is
  /// offline after three intervals without one.
  #[arg(long, default_value = "15s", value_parser = humantime::parse_duration)]
  heartbeat_interval: std::time::Duration,
  /// The control plane to join (runner mode): unix:/path, http://host:port
  /// or https://host:port. Defaults to the one it joined before.
  #[arg(long, value_name = "ENDPOINT")]
  join: Option<String>,
  /// Environment variable holding the join token (runner mode, first start
  /// only). Not a flag value, so it does not show in the process list.
  #[arg(long, value_name = "NAME", conflicts_with = "join_token_file")]
  join_token_env: Option<String>,
  /// File holding the join token (runner mode, first start only).
  #[arg(long, value_name = "PATH")]
  join_token_file: Option<PathBuf>,
  /// What this runner can host: actor, executor.
  #[arg(long, value_delimiter = ',', default_value = "executor")]
  roles: Vec<needy_core::types::RunnerRole>,
  /// This runner's labels: KEY=VALUE,...
  #[arg(long, value_delimiter = ',', value_parser = crate::commands::runner::label)]
  labels: Vec<(String, String)>,
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  serve(args, ctx).await.map_err(|error| match error.downcast_ref::<needy_api_server::server::Error>() {
    Some(needy_api_server::server::Error::Unauthenticated(_)) => anyhow!("{error}; set a token with --token-env NAME"),
    _ => error,
  })
}

async fn serve(args: Args, ctx: &Context) -> Result<i32> {
  if matches!(args.mode, Mode::Runner) && (args.listen.is_some() || args.token_env.is_some()) {
    bail!("a runner dials the control plane and listens nowhere; --listen and --token-env are for the executor and control modes");
  }
  let listen = match args.listen {
    Some(listen) => listen,
    None => ctx.endpoint()?,
  };
  // A token variable that is not set must not leave the daemon open.
  let token = match &args.token_env {
    Some(name) => match std::env::var(name) {
      Ok(token) if !token.is_empty() => Some(token),
      _ => bail!("--token-env {name}: {name} is not set"),
    },
    None => None,
  };
  match args.mode {
    Mode::Executor => needy_daemon::executor::serve(&listen, args.name.unwrap_or_else(|| "local".into()), token).await?,
    Mode::Runner => {
      let name = match args.name {
        Some(name) => name,
        None => host_name()?,
      };
      let config = needy_daemon::runner::Config {
        state_dir: ctx.paths()?.state,
        control: args.join,
        join_token: join_token(args.join_token_env.as_deref(), args.join_token_file.as_deref())?,
        name,
        roles: args.roles.into_iter().collect(),
        labels: args.labels.into_iter().collect(),
      };
      needy_daemon::runner::serve(config).await?;
    }
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
        kvm: needy_core_actor::host::vm::KVM.into(),
        docker_image: args.docker_image,
        heartbeat_interval: args.heartbeat_interval,
      })
      .await?;
      // Runs are stopped even when serving failed.
      daemon.serve(&listen, token).await?;
    }
  }
  Ok(0)
}

/// The join token from the variable or the file that holds it.
fn join_token(env: Option<&str>, file: Option<&std::path::Path>) -> Result<Option<String>> {
  let token = match (env, file) {
    (Some(name), _) => match std::env::var(name) {
      Ok(token) => token,
      Err(_) => bail!("--join-token-env {name}: {name} is not set"),
    },
    (None, Some(path)) => std::fs::read_to_string(path).with_context(|| format!("cannot read the join token from {}", path.display()))?,
    (None, None) => return Ok(None),
  };
  let token = token.trim().to_string();
  if token.is_empty() {
    bail!("the join token is empty");
  }
  Ok(Some(token))
}

/// A runner name from this machine's host name: its first label, in
/// lowercase, with characters a name cannot hold replaced by `-`.
fn host_name() -> Result<String> {
  let host = gethostname::gethostname().to_string_lossy().to_lowercase();
  let label = host.split('.').next().unwrap_or_default();
  let name: String = label.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).take(63).collect();
  let name = name.trim_matches('-').to_string();
  if name.is_empty() {
    bail!("cannot name this runner after the host name {host:?}; pass --name");
  }
  Ok(name)
}
