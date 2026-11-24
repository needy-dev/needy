//! Foreground serving and local OS service lifecycle.

mod runner;
mod service;

pub use runner::{RunnerArgs, join_runner, run_runner};

use crate::cli::Context;
#[cfg(unix)]
use anyhow::Context as _;
use anyhow::Result;

#[derive(clap::Args)]
pub struct Args {
  #[command(subcommand)]
  command: Command,
}

impl Args {
  /// Environment boundary for control serving, resolved before the CLI starts
  /// runtime/logger threads. Other daemon operations do not change it.
  pub fn environment(&self, ctx: &Context) -> Result<Vec<(std::ffi::OsString, std::ffi::OsString)>> {
    if !matches!(self.command, Command::Serve(_)) {
      return Ok(Vec::new());
    }
    let settings = ctx.configuration()?.daemon;
    let mut values = Vec::new();
    if let Some(host) = settings.engine_host {
      values.push(("DOCKER_HOST".into(), host.into()));
    }
    if let Some(entries) = settings.process_path {
      values.push(("PATH".into(), std::env::join_paths(entries)?));
    }
    Ok(values)
  }
}

#[derive(clap::Subcommand)]
enum Command {
  /// Serve the control plane in the foreground.
  Serve(ServeArgs),
  /// Own local actor and process resources independently of control.
  #[command(hide = true)]
  Holder(HolderArgs),
  /// Serve standalone executor processes on this machine.
  Executor(ExecutorArgs),
  /// Manage this machine's control service.
  Service(service::Args),
}

#[derive(clap::Args, Default)]
struct ServeArgs {
  /// Additional listener addresses (`tcp://HOST:PORT` or `unix:///PATH`); the private local socket always exists.
  #[arg(long)]
  listen: Vec<String>,
  /// Path to the base actor VM image directory.
  #[arg(long)]
  actor_image: Option<std::path::PathBuf>,
  /// Name for the built-in local runner on this control host.
  #[arg(long)]
  local_runner: Option<String>,
  // NOTICE: stand-in Firecracker tests use this path; real Firecracker
  // opens its platform-defined KVM device itself.
  #[arg(long, hide = true)]
  kvm: Option<std::path::PathBuf>,
  /// Path to the guest `needy` binary injected into VM actors.
  #[arg(long)]
  guest_needy: Option<std::path::PathBuf>,
  /// Default container image for `linux-container` executors.
  #[arg(long)]
  executor_image: Option<String>,
  /// Interval between runner health heartbeats (for example `15s`).
  #[arg(long, value_parser = humantime::parse_duration)]
  heartbeat_interval: Option<std::time::Duration>,
  /// Reject all `host:` file mappings (copy-in, mirror-out, sync, and host workspaces).
  #[arg(long)]
  disable_host_files: bool,
  /// Restrict `host:` file mappings to canonical paths inside this directory.
  #[arg(long, value_name = "DIR")]
  host_files_root: Option<std::path::PathBuf>,
  /// Block outbound actor gateway connections to this CIDR (repeatable, e.g. `10.0.0.0/8`).
  #[arg(long = "gateway-block-cidr", value_name = "CIDR")]
  gateway_blocked_cidrs: Vec<String>,
}

#[derive(clap::Args)]
struct HolderArgs {
  #[arg(long)]
  config_file: std::path::PathBuf,
}

#[derive(clap::Args)]
struct ExecutorArgs {
  /// Listener address (`tcp://HOST:PORT` or `unix:///PATH`).
  #[arg(long)]
  listen: String,
  /// Name reported for this standalone executor.
  #[arg(long, default_value = "local")]
  name: String,
  /// Private file containing the incoming executor bearer token.
  #[arg(long)]
  token_file: Option<std::path::PathBuf>,
  /// Share this executor's Unix socket with this group ID (mode 0660).
  #[arg(long)]
  socket_group: Option<u32>,
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  match args.command {
    Command::Serve(args) => serve(args, ctx).await,
    #[cfg(unix)]
    Command::Holder(args) => {
      needy_daemon::holder::serve_file(&args.config_file).await?;
      Ok(0)
    }
    #[cfg(not(unix))]
    Command::Holder(_) => anyhow::bail!("runtime holders require Unix"),
    Command::Executor(args) => {
      let token = match args.token_file {
        Some(path) => Some(crate::configuration::read_secret(&path)?),
        None => None,
      };
      if !args.listen.starts_with("unix:") && token.is_none() {
        anyhow::bail!("network executor serving requires --token-file FILE");
      }
      let listen = listener(&args.listen)?;
      needy_daemon::executor::serve(&listen, args.name, token, args.socket_group).await?;
      Ok(0)
    }
    Command::Service(args) => service::run(args, ctx).await,
  }
}

#[cfg(not(unix))]
async fn serve(_: ServeArgs, _: &Context) -> Result<i32> {
  Err(
    crate::diagnostics::Error::new("platform.unsupported", "Control hosting is not supported on Windows yet")
      .check("Use a Linux or macOS control host; Windows runners remain available.")
      .into(),
  )
}

#[cfg(unix)]
async fn serve(args: ServeArgs, ctx: &Context) -> Result<i32> {
  let paths = ctx.paths()?;
  let mut effective = needy_config::global::Config {
    daemon: ctx.configuration()?.daemon,
    ..Default::default()
  };
  if !args.listen.is_empty() {
    effective.daemon.listen = args.listen.clone();
  }
  if args.local_runner.is_some() {
    effective.daemon.local_runner = args.local_runner.clone();
  }
  if args.executor_image.is_some() {
    effective.daemon.executor_image = args.executor_image.clone();
  }
  if args.disable_host_files {
    effective.daemon.disable_host_files = true;
  }
  if args.host_files_root.is_some() {
    effective.daemon.host_files_root = args.host_files_root.clone();
  }
  if !args.gateway_blocked_cidrs.is_empty() {
    effective.daemon.gateway_blocked_cidrs = args.gateway_blocked_cidrs.clone();
  }
  effective.validate()?;
  if args.heartbeat_interval.is_some_and(|interval| interval.is_zero()) {
    return Err(
      crate::diagnostics::Error::new("config.invalid_value", "Heartbeat interval must be positive")
        .fact("field", "daemon.heartbeat_interval")
        .into(),
    );
  }
  let settings = effective.daemon;
  let config = needy_daemon::Config {
    holder_executable: std::env::current_exe().context("cannot locate the runtime holder executable")?,
    guest_disks: paths.guest_disks(),
    vm_images: paths.images(),
    actor_image: args.actor_image.or(settings.actor_image).unwrap_or_else(|| paths.base_image()),
    state_dir: paths.state,
    data_dir: paths.data.clone(),
    cache_dir: paths.cache,
    runtime_dir: paths.runtime,
    kvm: args.kvm.unwrap_or_else(|| needy_core_actor::driver::linux_firecracker::KVM.into()),
    guest_needy: args.guest_needy.or(settings.guest_needy),
    executor_image: args
      .executor_image
      .or(settings.executor_image)
      .unwrap_or_else(|| needy_core_executor::driver::linux_container::IMAGE.into()),
    heartbeat_interval: args.heartbeat_interval.unwrap_or(settings.heartbeat_interval.into()),
    local_runner: args.local_runner.or(settings.local_runner),
    report_wait: needy_daemon::REPORT_WAIT,
    mirror_out_wait: needy_daemon::MIRROR_OUT_WAIT,
    merge_driver_timeout: needy_daemon::MERGE_DRIVER_TIMEOUT,
    disable_host_files: args.disable_host_files || settings.disable_host_files,
    host_files_root: args.host_files_root.or(settings.host_files_root),
    gateway_blocked_cidrs: settings.gateway_blocked_cidrs,
  };
  match needy_daemon::guest_needy(&config) {
    Ok(guest) => tracing::info!("actors get the guest needy {}", guest.display()),
    Err(error) if config.guest_needy.is_some() => return Err(error),
    Err(error) => crate::output::message(format_args!("warning: {error:#}; actors cannot start until there is one")),
  }
  let listen = if args.listen.is_empty() {
    settings.listen
  } else {
    args.listen
  };
  let listen = listen.iter().map(|address| listener(address)).collect::<Result<Vec<_>>>()?;
  let progress = crate::output::Progress::new(ctx.show_progress(true), "Initializing control service...");
  let daemon = needy_daemon::Daemon::start(config).await?;
  progress.finish();
  // TODO(startup-feedback): listener readiness needs a serve callback before
  // progress can cover binding; keep persistent serving free of UI overlays.
  daemon.serve(&listen, None).await?;
  Ok(0)
}

fn listener(address: &str) -> Result<String> {
  needy_config::global::validate_address(address, true)?;
  Ok(address.strip_prefix("tcp://").map(str::to_string).unwrap_or_else(|| address.to_string()))
}
