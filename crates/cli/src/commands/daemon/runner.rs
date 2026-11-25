//! Runner startup shared by the runner command group.

use crate::cli::Context;
use anyhow::{Result, bail};
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct RunnerArgs {
  #[arg(long)]
  pub(crate) name: Option<String>,
  #[arg(long)]
  pub(crate) address: Option<String>,
  #[arg(long)]
  pub(crate) actor_image: Option<PathBuf>,
  #[arg(long)]
  pub(crate) guest_needy: Option<PathBuf>,
  #[arg(long)]
  pub(crate) kvm: Option<PathBuf>,
  #[arg(long)]
  pub(crate) work_dir: Option<PathBuf>,
  #[arg(long)]
  pub(crate) gateway: Option<String>,
  #[arg(long)]
  pub(crate) gateway_upstream: Option<String>,
  #[arg(long, requires = "gateway_upstream")]
  pub(crate) gateway_upstream_credentials_file: Option<PathBuf>,
  #[arg(long)]
  pub(crate) ephemeral: bool,
  #[arg(long)]
  pub(crate) join: Option<String>,
  #[arg(long, conflicts_with = "join_token_file")]
  pub(crate) join_token_env: Option<String>,
  #[arg(long)]
  pub(crate) join_token_file: Option<PathBuf>,
  #[arg(long, conflicts_with = "credential_file")]
  pub(crate) credential_env: Option<String>,
  #[arg(long, conflicts_with = "credential_env")]
  pub(crate) credential_file: Option<PathBuf>,
  #[arg(long, value_delimiter = ',')]
  pub(crate) roles: Option<Vec<needy_core::types::RunnerRole>>,
  #[arg(long, value_delimiter = ',', value_parser = crate::commands::runner::label)]
  pub(crate) labels: Vec<(String, String)>,
  #[arg(long, hide = true, value_parser = humantime::parse_duration)]
  pub(crate) disconnect_grace: Option<std::time::Duration>,
  #[arg(long)]
  pub(crate) activity_hook: Option<String>,
  #[arg(long, value_parser = humantime::parse_duration)]
  pub(crate) activity_hook_interval: Option<std::time::Duration>,
}

/// `runner join` enrolls through the daemon boundary, saves its credential,
/// and reports how to start serving; it does not open persistent work streams.
pub async fn join_runner(args: RunnerArgs, ctx: &Context, code: String) -> Result<i32> {
  execute(args, ctx, Some(code)).await
}

pub async fn run_runner(args: RunnerArgs, ctx: &Context) -> Result<i32> {
  execute(args, ctx, None).await
}

async fn execute(mut args: RunnerArgs, ctx: &Context, code: Option<String>) -> Result<i32> {
  let joining = code.is_some();
  let labels_overridden = !args.labels.is_empty();
  let credential = read_runner_credential(args.credential_env.as_deref(), args.credential_file.as_deref())?;
  let stateless = credential.is_some();
  let control = args.address.take().or_else(|| args.join.take());
  if let Some(address) = &control
    && !address.starts_with("exec:")
  {
    needy_config::global::validate_address(address, false)?;
  }
  if stateless {
    if control.is_none() {
      bail!("--address URI is required when using --credential-env or --credential-file");
    }
    if args.name.is_none() {
      let cred = credential.as_deref().unwrap_or_default();
      args.name = Some(
        needy_daemon::runner::credential_name(cred)
          .map(str::to_string)
          .ok_or_else(|| anyhow::anyhow!("cannot derive runner name from credential; pass --name NAME"))?,
      );
    }
  } else if !joining {
    let (name, stored) = needy_daemon::runner::enrollment(&ctx.paths()?.state)?;
    args.name = Some(name);
    if let Some(roles) = &args.roles {
      if let Some(granted) = &stored.roles
        && roles.iter().any(|role| !granted.contains(role))
      {
        bail!("runner serve --roles may narrow the enrolled roles, not expand them");
      }
    } else {
      args.roles = stored.roles.map(|roles| roles.into_iter().collect());
    }
    if args.labels.is_empty() {
      args.labels = stored.labels.into_iter().collect();
    }
    if args.actor_image.is_none() {
      args.actor_image = stored.actor_image;
    }
  }
  if let Some(image) = args.actor_image {
    args.actor_image = Some(std::fs::canonicalize(image)?);
  }
  let activity_hook = match args.activity_hook {
    Some(command) => {
      anyhow::ensure!(!command.trim().is_empty(), "--activity-hook command must not be empty");
      let interval = args.activity_hook_interval.unwrap_or(needy_daemon::runner::DEFAULT_ACTIVITY_HOOK_INTERVAL);
      anyhow::ensure!(!interval.is_zero(), "--activity-hook-interval must be positive");
      Some(needy_daemon::runner::ActivityHookConfig { command, interval })
    }
    None => None,
  };
  let settings = needy_daemon::runner::Settings {
    roles: args.roles.clone().map(|roles| roles.into_iter().collect()),
    labels: args.labels.clone().into_iter().collect(),
    actor_image: args.actor_image.clone(),
  };
  let name = match args.name {
    Some(name) => name,
    None => host_name()?,
  };
  let paths = ctx.paths()?;
  let image = args.actor_image.unwrap_or_else(|| paths.base_image());
  let kvm = args.kvm.unwrap_or_else(|| needy_core_actor::driver::linux_firecracker::KVM.into());
  let roles: Option<std::collections::BTreeSet<_>> = args.roles.map(|roles| roles.into_iter().collect());
  let progress = crate::output::Progress::new(joining && ctx.show_progress(true), "Checking local runner capabilities");
  let (capabilities, warnings) = needy_daemon::capabilities::of_this_machine(&kvm, &image, roles.as_ref()).await;
  progress.finish();
  for warning in &warnings {
    crate::output::message(format_args!("warning: {warning}"));
  }
  if let Some(roles) = &roles
    && roles.iter().any(|role| match role {
      needy_core::types::RunnerRole::Actor => capabilities.actor.is_empty(),
      needy_core::types::RunnerRole::Executor => capabilities.executor.is_empty(),
    })
  {
    bail!("an explicitly requested runner role is unavailable on this machine; inspect the capability warnings");
  }
  if capabilities.is_empty() {
    bail!("this machine can host none of the roles --roles names");
  }
  let capability_summary = capabilities.to_string();
  let control_summary = match control.as_deref() {
    Some(address) if address.starts_with("exec:") => "saved connector".to_string(),
    Some(address) => address.to_string(),
    None => "saved control connection".to_string(),
  };
  #[cfg(unix)]
  let gateway_upstream = gateway_upstream(args.gateway_upstream.as_deref(), args.gateway_upstream_credentials_file).await?;
  #[cfg(unix)]
  let actor = (!capabilities.actor.is_empty()).then(|| needy_daemon::runner::ActorConfig {
    gateway: args.gateway.clone(),
    actor_image: image.clone(),
    guest_disks: paths.guest_disks(),
    layer_store: needy_daemon::runner::layer_store(&paths.cache, &paths.data),
    guest_needy: args.guest_needy.clone(),
    kvm: kvm.clone(),
    work_dir: args.work_dir.clone(),
    gateway_upstream,
  });
  let config = needy_daemon::runner::Config {
    state_dir: paths.state,
    control,
    join_token: code,
    credential,
    name: name.clone(),
    capabilities,
    labels: args.labels.into_iter().collect(),
    grace: args.disconnect_grace.unwrap_or(needy_daemon::runner::DISCONNECT_GRACE),
    activity_hook,
    ephemeral: args.ephemeral,
    #[cfg(unix)]
    actor,
  };
  if joining {
    let progress = crate::output::Progress::new(
      ctx.show_progress(true) && !config.control.as_deref().is_some_and(|control| control.starts_with("exec:")),
      format!("Contacting control daemon: {control_summary}"),
    );
    // Keep the existing plain contact line on stderr when terminal animation is hidden.
    if !progress.is_visible() {
      crate::output::detail(format_args!("Contacting control daemon: {control_summary}"))?;
    }
    // An enrollment code is consumed remotely. The first signal must let its
    // response reach private storage, as the former combined join/serve did.
    let termination = needy_api_server::termination::Termination::listen()?;
    match termination.unless_forced(needy_daemon::runner::enroll(config, settings)).await {
      Some(result) => result?,
      None => bail!(
        "a second termination signal ended enrollment before its credential was saved; inspect the runner registration before retrying"
      ),
    }
    progress.finish();
    crate::output::acknowledge("Joined successfully. Enrollment saved.")?;
    crate::output::detail(format_args!(
      "\n  {:<20}{name}\n  {:<20}{control_summary}\n  {:<20}{capability_summary}\n\nOn this machine, start the runner and keep it connected:\n\n  needy runner serve\n\nOn the control host, inspect runners with:\n\n  needy runner ls\n\nFor an OS-managed control daemon, inspect its status and logs with:\n\n  needy daemon service status\n  needy daemon service logs --follow",
      "Runner:", "Control daemon:", "Local capabilities:"
    ))?;
  } else {
    // TODO(runner-progress): persistent serve has no connection-state callback;
    // add waiting feedback only with an owner-approved callback that separates
    // connection/reconnection from active serving, without covering its lifetime.
    needy_daemon::runner::serve_with_labels(config, labels_overridden && !stateless).await?;
  }
  Ok(0)
}

/// The actor gateway's upstream `url` names, with the credentials in
/// `credentials`, checked now: a bad URL or credentials file fails the
/// start instead of every connection. Shared by `daemon serve` and
/// `runner serve`.
pub(crate) async fn gateway_upstream(
  url: Option<&str>,
  credentials: Option<PathBuf>,
) -> Result<needy_core_actor::sidecar::gateway::Upstream> {
  use needy_core_actor::sidecar::gateway::Upstream;
  let Some(url) = url else {
    return Ok(Upstream::Direct);
  };
  let invalid = |error: needy_core_actor::sidecar::gateway::UpstreamError| {
    anyhow::Error::from(
      crate::diagnostics::Error::new("config.invalid_value", format!("Invalid gateway upstream: {error}"))
        .fact("field", "--gateway-upstream")
        .check("Pass socks5h://HOST[:PORT] or socks5://HOST[:PORT], and USERNAME:PASSWORD in a file other users cannot read."),
    )
  };
  let upstream = Upstream::socks5(url, credentials).map_err(invalid)?;
  upstream.check().await.map_err(invalid)?;
  Ok(upstream)
}

/// Reads a pre-provisioned runner credential from an environment variable or
/// read-only file (such as a Kubernetes mounted Secret).
fn read_runner_credential(env: Option<&str>, file: Option<&std::path::Path>) -> Result<Option<String>> {
  let raw = match (env, file) {
    (Some(var), None) => std::env::var(var).map_err(|_| anyhow::anyhow!("environment variable {var:?} is not set"))?,
    (None, Some(path)) => {
      std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("cannot read runner credential file {}: {e}", path.display()))?
    }
    (None, None) => return Ok(None),
    (Some(_), Some(_)) => bail!("set only one of --credential-env or --credential-file"),
  };
  let trimmed = raw.trim();
  anyhow::ensure!(!trimmed.is_empty() && !trimmed.contains(['\r', '\n', '\0']), "runner credential must be a single nonempty line");
  Ok(Some(trimmed.to_string()))
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
