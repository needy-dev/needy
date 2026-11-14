//! Runner startup shared by the runner command group.

use crate::cli::Context;
use anyhow::{Result, bail};
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct RunnerArgs {
  #[arg(long)]
  pub(crate) name: Option<String>,
  #[arg(long)]
  pub(crate) actor_image: Option<PathBuf>,
  #[arg(long, hide = true)]
  pub(crate) kvm: Option<PathBuf>,
  #[arg(long)]
  pub(crate) join: Option<String>,
  #[arg(long, conflicts_with = "join_token_file")]
  pub(crate) join_token_env: Option<String>,
  #[arg(long)]
  pub(crate) join_token_file: Option<PathBuf>,
  #[arg(long, value_delimiter = ',')]
  pub(crate) roles: Option<Vec<needy_core::types::RunnerRole>>,
  #[arg(long, value_delimiter = ',', value_parser = crate::commands::runner::label)]
  pub(crate) labels: Vec<(String, String)>,
  #[arg(long, hide = true, value_parser = humantime::parse_duration)]
  pub(crate) disconnect_grace: Option<std::time::Duration>,
}

pub async fn join_runner(args: RunnerArgs, ctx: &Context, code: String) -> Result<i32> {
  execute(args, ctx, Some(code)).await
}

pub async fn run_runner(args: RunnerArgs, ctx: &Context) -> Result<i32> {
  execute(args, ctx, None).await
}

async fn execute(mut args: RunnerArgs, ctx: &Context, code: Option<String>) -> Result<i32> {
  let joining = code.is_some();
  let labels_overridden = !args.labels.is_empty();
  if !joining {
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
  let (capabilities, warnings) = needy_daemon::capabilities::of_this_machine(&kvm, &image, roles.as_ref()).await;
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
  let config = needy_daemon::runner::Config {
    state_dir: paths.state,
    control: args.join,
    join_token: code,
    name,
    capabilities,
    labels: args.labels.into_iter().collect(),
    grace: args.disconnect_grace.unwrap_or(needy_daemon::runner::DISCONNECT_GRACE),
  };
  if joining {
    // An enrollment code is consumed remotely. The first signal must let its
    // response reach private storage, as the former combined join/serve did.
    let termination = needy_api_server::termination::Termination::listen()?;
    match termination.unless_forced(needy_daemon::runner::enroll(config, settings)).await {
      Some(result) => result?,
      None => bail!(
        "a second termination signal ended enrollment before its credential was saved; inspect the runner registration before retrying"
      ),
    }
  } else {
    needy_daemon::runner::serve_with_labels(config, labels_overridden).await?;
  }
  Ok(0)
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
