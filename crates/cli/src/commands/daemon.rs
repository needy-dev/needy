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
  /// Environment variable holding the control token: the bootstrap and
  /// admin credential, with which an operator also pairs clients (`needy
  /// client pair create`); paired clients present their own tokens.
  /// Required to listen on a TCP address other than loopback.
  #[arg(long)]
  token_env: Option<String>,
  /// Directory with the actor's vmlinux, rootfs.ext4 and firecracker.
  /// Defaults to the image `needy build` writes. A runner holds the actor
  /// role with the firecracker runtime only when it is complete.
  #[arg(long)]
  actor_image: Option<PathBuf>,
  /// The runner that runs on this host (control mode). The control plane
  /// starts actors placed on it and creates its Docker executors itself;
  /// actors and Docker executors go nowhere else yet.
  #[arg(long, value_name = "NAME")]
  local_runner: Option<String>,
  /// The KVM device checked before an actor starts (control mode), and to
  /// detect the firecracker runtime (runner mode).
  // NOTICE: Firecracker opens /dev/kvm itself; another path only makes sense
  // with a stand-in Firecracker from --actor-image, as the CLI's tests use on
  // hosts without KVM. Hidden for that reason.
  #[arg(long, value_name = "PATH", hide = true)]
  kvm: Option<PathBuf>,
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
  /// Restrict this runner to these roles: actor, executor. Without it the
  /// runner holds every role it detects (a usable KVM and the actor image
  /// for actors; native, and Docker when it answers, for executors). A
  /// role this machine cannot host is warned about and left out.
  #[arg(long, value_delimiter = ',')]
  roles: Option<Vec<needy_core::types::RunnerRole>>,
  /// This runner's labels: KEY=VALUE,...
  #[arg(long, value_delimiter = ',', value_parser = crate::commands::runner::label)]
  labels: Vec<(String, String)>,
  /// How long a runner that cannot reach the control plane keeps the
  /// processes of runs before it ends them (runner mode).
  // NOTICE: hidden: the default (`needy_daemon::runner::DISCONNECT_GRACE`)
  // is the design's; a shorter one lets a test see the runner end them.
  #[arg(long, value_name = "DURATION", hide = true, value_parser = humantime::parse_duration)]
  disconnect_grace: Option<std::time::Duration>,
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  serve(args, ctx).await.map_err(|error| match error.downcast_ref::<needy_api_server::server::Error>() {
    Some(needy_api_server::server::Error::Unauthenticated(_)) => anyhow!("{error}; set a token with --token-env NAME"),
    _ => error,
  })
}

async fn serve(args: Args, ctx: &Context) -> Result<i32> {
  if !matches!(args.mode, Mode::Control) && args.local_runner.is_some() {
    bail!("--local-runner is for the control mode");
  }
  if matches!(args.mode, Mode::Executor) && args.kvm.is_some() {
    bail!("--kvm is for the control and runner modes");
  }
  if !matches!(args.mode, Mode::Runner) && args.roles.is_some() {
    bail!("--roles is for the runner mode");
  }
  if !matches!(args.mode, Mode::Runner) && args.disconnect_grace.is_some() {
    bail!("--disconnect-grace is for the runner mode");
  }
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
      let paths = ctx.paths()?;
      let image = args.actor_image.unwrap_or_else(|| paths.default_image());
      let kvm = args.kvm.unwrap_or_else(|| needy_core_actor::host::vm::KVM.into());
      let roles: Option<std::collections::BTreeSet<_>> = args.roles.map(|roles| roles.into_iter().collect());
      let (capabilities, warnings) = needy_daemon::capabilities::of_this_machine(&kvm, &image, roles.as_ref()).await;
      for warning in &warnings {
        crate::output::message(format_args!("warning: {warning}"));
      }
      if capabilities.is_empty() {
        bail!("this machine can host none of the roles --roles names");
      }
      let config = needy_daemon::runner::Config {
        state_dir: paths.state,
        control: args.join,
        join_token: join_token(args.join_token_env.as_deref(), args.join_token_file.as_deref())?,
        name,
        capabilities,
        labels: args.labels.into_iter().collect(),
        grace: args.disconnect_grace.unwrap_or(needy_daemon::runner::DISCONNECT_GRACE),
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
        kvm: args.kvm.unwrap_or_else(|| needy_core_actor::host::vm::KVM.into()),
        docker_image: args.docker_image,
        heartbeat_interval: args.heartbeat_interval,
        local_runner: args.local_runner,
        report_wait: needy_daemon::REPORT_WAIT,
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
