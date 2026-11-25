//! `needy runner`: join tokens and the runners that joined with them.
//! A runner enrolls with `runner join`, then connects with `runner serve`.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use clap::Subcommand;
use needy_api_client::proto::v1::{
  self, CreateJoinTokenRequest, ListJoinTokensRequest, ListRunnersRequest, RemoveRunnerRequest, RevokeJoinTokenRequest, RotateRunnerRequest,
};
use needy_config::profile::{ActorDriver, ExecutorDriver};
use needy_core::types::{Capabilities, RunnerRole};
use serde::Serialize;

use crate::cli::{ConnectionArgs, Context};
use crate::output::{self, OutputFormat, Row, ago, outln, time};

#[derive(clap::Args)]
pub struct Args {
  #[command(subcommand)]
  command: Command,
}

#[derive(Subcommand)]
enum Command {
  /// Issue a runner invitation (the token is shown once).
  Invite(InviteArgs),
  /// Enroll this machine as a runner and save its credential.
  Join(JoinArgs),
  /// Connect and serve work using this machine's saved runner enrollment.
  Serve(ServeArgs),
  /// Inspect or revoke runner join tokens.
  Token {
    #[command(flatten)]
    connection: ConnectionArgs,
    #[command(subcommand)]
    command: TokenCommand,
  },
  /// List registered runners, including removed machines.
  Ls {
    /// Output format.
    #[arg(short = 'o', long, value_enum, default_value_t)]
    output: OutputFormat,
    #[command(flatten)]
    connection: ConnectionArgs,
  },
  /// Rotate a runner's credential on its next heartbeat.
  Rotate {
    /// Runner name to rotate.
    name: String,
    #[command(flatten)]
    connection: ConnectionArgs,
  },
  /// Revoke a runner while keeping its registry record.
  Rm {
    /// Runner name to revoke.
    name: String,
    #[command(flatten)]
    connection: ConnectionArgs,
  },
}

impl Args {
  pub(crate) fn connection(&self) -> ConnectionArgs {
    match &self.command {
      Command::Invite(args) => args.connection.clone(),
      Command::Join(args) => args.connection.clone(),
      Command::Serve(_) => ConnectionArgs::default(),
      Command::Token { connection, .. }
      | Command::Ls { connection, .. }
      | Command::Rotate { connection, .. }
      | Command::Rm { connection, .. } => connection.clone(),
    }
  }
}

#[derive(clap::Args)]
struct InviteArgs {
  /// Time until the invitation expires (for example `1h` or `30m`).
  #[arg(long, default_value = "1h", value_parser = humantime::parse_duration)]
  ttl: Duration,
  /// Maximum number of runners that may enroll with this token.
  #[arg(long, default_value_t = 1)]
  uses: u32,
  /// Roles the enrolled runner is allowed to host (comma-separated: `actor`, `executor`).
  #[arg(long, value_delimiter = ',')]
  roles: Vec<RunnerRole>,
  /// Labels applied to enrolled runners (`KEY=VALUE`, comma-separated).
  #[arg(long, value_delimiter = ',', value_parser = label)]
  labels: Vec<(String, String)>,
  /// Output format.
  #[arg(short = 'o', long, value_enum, default_value_t)]
  output: OutputFormat,
  #[command(flatten)]
  connection: ConnectionArgs,
}

#[derive(clap::Args)]
struct JoinArgs {
  /// Name for this runner on the control plane (must be a DNS label).
  name: String,
  /// Read the joining token from a private file, or `-` for stdin.
  // NOTICE: Files and stdin cover automation in this slice. Token-valued argv
  // and implicit environment inputs need a separate owner-approved input contract.
  #[arg(long)]
  code_file: Option<std::path::PathBuf>,
  /// Roles this runner should host (comma-separated: `actor`, `executor`).
  #[arg(long, value_delimiter = ',')]
  roles: Option<Vec<RunnerRole>>,
  /// Labels advertised by this runner (`KEY=VALUE`, comma-separated).
  #[arg(long, value_delimiter = ',', value_parser = label)]
  labels: Vec<(String, String)>,
  /// Path to the base actor VM image directory on this machine.
  #[arg(long)]
  actor_image: Option<std::path::PathBuf>,
  #[arg(long, hide = true)]
  kvm: Option<std::path::PathBuf>,
  #[command(flatten)]
  connection: ConnectionArgs,
  /// Connector command after `--`; conflicts with `--address` or `--context`.
  #[arg(last = true, allow_hyphen_values = true)]
  command: Vec<String>,
}

// TODO(runner-registry): `--insecure-registry` (and the CA and credential
// conventions of `oci-registry`) are not offered here yet: a runner makes no
// registry requests until B-5's direct layer pulls, so the flag would change
// nothing. Add it with them (layered OCI base spec, "Settled in B-3").
#[derive(clap::Args)]
struct ServeArgs {
  /// Control daemon address (`tcp://HOST:PORT`, `http://HOST:PORT`, `https://HOST:PORT`, or `unix:///PATH`).
  #[arg(long)]
  address: Option<String>,
  /// Runner name (optional when using `--credential-env` or `--credential-file`; extracted from the credential by default).
  #[arg(long)]
  name: Option<String>,
  /// Read a pre-provisioned runner credential from an environment variable (read-only; never writes `runner.json`).
  #[arg(long, conflicts_with = "credential_file")]
  credential_env: Option<String>,
  /// Read a pre-provisioned runner credential from a file (read-only; never writes `runner.json`).
  #[arg(long, conflicts_with = "credential_env")]
  credential_file: Option<std::path::PathBuf>,
  /// Narrow the enrolled roles this runner hosts (comma-separated: `actor`, `executor`).
  #[arg(long, value_delimiter = ',')]
  roles: Option<Vec<RunnerRole>>,
  /// Override labels advertised by this runner (`KEY=VALUE`, comma-separated).
  #[arg(long, value_delimiter = ',', value_parser = label)]
  labels: Vec<(String, String)>,
  /// Path to the base actor VM image directory on this machine.
  #[arg(long)]
  actor_image: Option<std::path::PathBuf>,
  /// Path to the guest `needy` binary injected into actor VMs.
  #[arg(long)]
  guest_needy: Option<std::path::PathBuf>,
  /// Path to the KVM device (defaults to `/dev/kvm`).
  #[arg(long)]
  kvm: Option<std::path::PathBuf>,
  /// Directory under which each actor run gets its own directory for temporary VM files (`vm.json`, `v.sock`, `console.log`); uses a temp dir if omitted.
  #[arg(long)]
  work_dir: Option<std::path::PathBuf>,
  /// Control-plane shared TCP gateway address (`HOST:PORT` or `tcp://HOST:PORT`) for remote actor runs, used only with a
  /// control plane that does not let this runner run the actor's gateway itself (one older than this runner).
  #[arg(long, alias = "gateway-address")]
  gateway: Option<String>,
  /// Upstream SOCKS5 proxy for the actor gateway this runner runs: `socks5h://HOST[:PORT]` (the proxy resolves names) or
  /// `socks5://HOST[:PORT]` (this runner resolves and checks them); port 1080 by default. Never falls back to direct.
  #[arg(long, value_name = "URL")]
  gateway_upstream: Option<String>,
  /// File holding `USERNAME:PASSWORD` for `--gateway-upstream`, read again for each connection (a rotated Secret
  /// applies); other users must not be able to read it (0600, or 0440 with a Pod's fsGroup).
  #[arg(long, value_name = "PATH", requires = "gateway_upstream")]
  gateway_upstream_credentials_file: Option<std::path::PathBuf>,
  /// Serve a single actor run (or a single bound run in executor mode) and exit cleanly.
  #[arg(long)]
  ephemeral: bool,
  #[arg(long, hide = true, value_parser = humantime::parse_duration)]
  disconnect_grace: Option<Duration>,
  /// Shell command invoked asynchronously (throttled) when routed exec or SFTP streams are active.
  #[arg(long)]
  activity_hook: Option<String>,
  /// Minimum interval between `--activity-hook` invocations (default `15s`).
  #[arg(long, value_parser = humantime::parse_duration)]
  activity_hook_interval: Option<Duration>,
}

#[derive(Subcommand)]
enum TokenCommand {
  /// List join-token metadata, including spent and revoked invitations.
  Ls {
    /// Output format.
    #[arg(short = 'o', long, value_enum, default_value_t)]
    output: OutputFormat,
  },
  /// Revoke a join token without affecting already-enrolled runners.
  Revoke {
    /// Join token ID to revoke.
    id: String,
  },
}

/// One `KEY=VALUE` label.
pub fn label(text: &str) -> Result<(String, String), String> {
  match text.split_once('=') {
    Some((key, value)) if !key.is_empty() => Ok((key.to_string(), value.to_string())),
    _ => Err(format!("a label is KEY=VALUE, not {text:?}")),
  }
}

/// A role as the wire carries it.
pub fn proto_role(role: RunnerRole) -> i32 {
  match role {
    RunnerRole::Actor => v1::RunnerRole::Actor as i32,
    RunnerRole::Executor => v1::RunnerRole::Executor as i32,
  }
}

/// The role a message names. Its zero value means the daemon left it
/// unset, and a value this build does not know comes from a newer daemon;
/// both are errors instead of a default.
fn role(value: i32) -> Result<RunnerRole> {
  match v1::RunnerRole::try_from(value) {
    Ok(v1::RunnerRole::Unspecified) => bail!("the runner role is unspecified"),
    Ok(v1::RunnerRole::Actor) => Ok(RunnerRole::Actor),
    Ok(v1::RunnerRole::Executor) => Ok(RunnerRole::Executor),
    Err(_) => bail!("unknown runner role {value}"),
  }
}

/// What a runner reported it can host, from the wire. Unspecified and
/// unknown runtimes and providers are errors, as roles are.
fn capabilities(reported: Option<v1::RunnerCapabilities>) -> Result<Capabilities> {
  let reported = reported.unwrap_or_default();
  let actor = reported
    .actor
    .iter()
    .map(|&value| match v1::ActorDriver::try_from(value) {
      Ok(v1::ActorDriver::Unspecified) => bail!("the actor driver is unspecified"),
      Ok(v1::ActorDriver::LinuxFirecracker) => Ok(ActorDriver::LinuxFirecracker),
      Ok(v1::ActorDriver::LinuxContainer) => Ok(ActorDriver::LinuxContainer),
      Ok(v1::ActorDriver::MacosVz) => Ok(ActorDriver::MacosVz),
      Ok(v1::ActorDriver::MacosAppleContainer) => Ok(ActorDriver::MacosAppleContainer),
      Err(_) => bail!("unknown actor driver {value}"),
    })
    .collect::<Result<_>>()?;
  let executor = reported
    .executor
    .iter()
    .map(|&value| match v1::ExecutorDriver::try_from(value) {
      Ok(v1::ExecutorDriver::Unspecified) => bail!("the executor driver is unspecified"),
      Ok(v1::ExecutorDriver::Native) => Ok(ExecutorDriver::Native),
      Ok(v1::ExecutorDriver::LinuxContainer) => Ok(ExecutorDriver::LinuxContainer),
      Err(_) => bail!("unknown executor driver {value}"),
    })
    .collect::<Result<_>>()?;
  Ok(Capabilities { actor, executor })
}

fn roles(values: &[i32]) -> Result<Vec<String>> {
  let mut roles = values.iter().map(|&v| role(v).map(|r| r.to_string())).collect::<Result<Vec<_>>>()?;
  roles.sort();
  Ok(roles)
}

fn token_state(value: i32) -> Result<&'static str> {
  match v1::JoinTokenState::try_from(value) {
    Ok(v1::JoinTokenState::Unspecified) => bail!("the join token state is unspecified"),
    Ok(v1::JoinTokenState::Valid) => Ok("valid"),
    Ok(v1::JoinTokenState::Expired) => Ok("expired"),
    Ok(v1::JoinTokenState::UsedUp) => Ok("used-up"),
    Ok(v1::JoinTokenState::Revoked) => Ok("revoked"),
    Err(_) => bail!("unknown join token state {value}"),
  }
}

fn runner_status(value: i32) -> Result<&'static str> {
  match v1::RunnerStatus::try_from(value) {
    Ok(v1::RunnerStatus::Unspecified) => bail!("the runner status is unspecified"),
    Ok(v1::RunnerStatus::Online) => Ok("online"),
    Ok(v1::RunnerStatus::Offline) => Ok("offline"),
    Ok(v1::RunnerStatus::Removed) => Ok("removed"),
    Err(_) => bail!("unknown runner status {value}"),
  }
}

fn labels_cell(labels: &BTreeMap<String, String>) -> String {
  if labels.is_empty() {
    return "-".into();
  }
  labels.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(",")
}

#[derive(Serialize)]
struct TokenRow {
  id: String,
  state: String,
  uses_left: u32,
  created_at_ms: i64,
  expires_at_ms: i64,
  /// Empty allows every role.
  roles: Vec<String>,
  labels: BTreeMap<String, String>,
}

impl Row for TokenRow {
  const HEADER: &'static [&'static str] = &["ID", "STATE", "USES LEFT", "EXPIRES", "ROLES", "LABELS"];

  fn cells(&self) -> Vec<String> {
    let roles = if self.roles.is_empty() {
      "any".into()
    } else {
      self.roles.join(",")
    };
    vec![
      self.id.clone(),
      self.state.clone(),
      self.uses_left.to_string(),
      time(self.expires_at_ms),
      roles,
      labels_cell(&self.labels),
    ]
  }
}

impl TryFrom<v1::JoinToken> for TokenRow {
  type Error = anyhow::Error;

  fn try_from(token: v1::JoinToken) -> Result<Self> {
    Ok(Self {
      state: token_state(token.state)?.into(),
      roles: roles(&token.roles)?,
      id: token.id,
      uses_left: token.uses_left,
      created_at_ms: token.created_at_ms,
      expires_at_ms: token.expires_at_ms,
      labels: token.labels.into_iter().collect(),
    })
  }
}

/// A runner's machine as `runner ls` shows it.
#[derive(Serialize)]
struct HostRow {
  os: String,
  arch: String,
  /// Empty off Linux.
  cpu_vendor: String,
  cpu_model: String,
  /// Whether KVM answered with its supported CPUID entries.
  kvm: bool,
}

#[derive(Serialize)]
struct RunnerRow {
  name: String,
  status: String,
  /// A new credential was asked for and the runner has not used it yet.
  rotating: bool,
  /// Each role it holds with its runtimes or providers.
  capabilities: Capabilities,
  /// Unset until the runner's first heartbeat to this control plane.
  host: Option<HostRow>,
  labels: BTreeMap<String, String>,
  created_at_ms: i64,
  last_seen_at_ms: i64,
  join_token_id: String,
}

impl Row for RunnerRow {
  const HEADER: &'static [&'static str] = &[
    "NAME",
    "STATUS",
    "CAPABILITIES",
    "HOST",
    "CPU",
    "LABELS",
    "LAST SEEN",
  ];

  fn cells(&self) -> Vec<String> {
    let status = if self.rotating {
      format!("{} (rotating)", self.status)
    } else {
      self.status.clone()
    };
    let (host, cpu) = match &self.host {
      Some(host) if host.cpu_model.is_empty() => (format!("{}/{}", host.os, host.arch), "-".to_string()),
      Some(host) => (format!("{}/{}", host.os, host.arch), host.cpu_model.clone()),
      None => ("-".into(), "-".into()),
    };
    vec![
      self.name.clone(),
      status,
      self.capabilities.to_string(),
      host,
      cpu,
      labels_cell(&self.labels),
      ago(self.last_seen_at_ms),
    ]
  }
}

impl TryFrom<v1::Runner> for RunnerRow {
  type Error = anyhow::Error;

  fn try_from(runner: v1::Runner) -> Result<Self> {
    Ok(Self {
      status: runner_status(runner.status)?.into(),
      capabilities: capabilities(runner.capabilities)?,
      host: runner.host.map(|host| HostRow {
        kvm: host.kvm_cpuid.is_some(),
        os: host.os,
        arch: host.arch,
        cpu_vendor: host.cpu_vendor,
        cpu_model: host.cpu_model,
      }),
      name: runner.name,
      rotating: runner.rotating,
      labels: runner.labels.into_iter().collect(),
      created_at_ms: runner.created_at_ms,
      last_seen_at_ms: runner.last_seen_at_ms,
      join_token_id: runner.join_token_id,
    })
  }
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  match args.command {
    Command::Join(args) => return join(args, ctx).await,
    Command::Serve(args) => {
      return super::daemon::run_runner(
        super::daemon::RunnerArgs {
          name: args.name,
          address: args.address,
          actor_image: args.actor_image,
          guest_needy: args.guest_needy,
          kvm: args.kvm,
          work_dir: args.work_dir,
          gateway: args.gateway,
          gateway_upstream: args.gateway_upstream,
          gateway_upstream_credentials_file: args.gateway_upstream_credentials_file,
          ephemeral: args.ephemeral,
          join: None,
          join_token_env: None,
          join_token_file: None,
          credential_env: args.credential_env,
          credential_file: args.credential_file,
          roles: args.roles,
          labels: args.labels,
          disconnect_grace: args.disconnect_grace,
          activity_hook: args.activity_hook,
          activity_hook_interval: args.activity_hook_interval,
        },
        ctx,
      )
      .await;
    }
    command => management(command, ctx).await,
  }
}

async fn join(args: JoinArgs, ctx: &Context) -> Result<i32> {
  anyhow::ensure!(needy_config::global::valid_name(&args.name), "runner name must be a DNS label");
  anyhow::ensure!(ctx.credential_file.is_none(), "runner join uses its invitation, not --credential-file");
  let count = usize::from(ctx.address.is_some()) + usize::from(ctx.selected_context.is_some()) + usize::from(!args.command.is_empty());
  anyhow::ensure!(count == 1, "runner join requires exactly one --address URI, --context NAME transport or -- PROGRAM ARG...");
  let address = if !args.command.is_empty() {
    anyhow::ensure!(!args.command[0].is_empty() && !args.command.iter().any(|a| a.contains('\0')), "connector argv is invalid");
    format!("exec:{}", shell_words::join(&args.command))
  } else if let Some(address) = &ctx.address {
    needy_config::global::validate_address(address, false)?;
    address.clone()
  } else {
    let config = ctx.configuration()?;
    let name = ctx.selected_context.as_ref().unwrap();
    if name == "local" {
      #[cfg(unix)]
      {
        needy_daemon::discovery::read(&ctx.paths()?.runtime)?.address
      }
      #[cfg(not(unix))]
      {
        bail!("local control hosting is not supported on this platform yet");
      }
    } else {
      let record = config.contexts.get(name).ok_or_else(|| anyhow::anyhow!("unknown context {name:?}; use `needy config context ls`"))?;
      match (&record.address, &record.command) {
        (Some(address), None) => address.clone(),
        (None, Some(command)) => format!("exec:{}", shell_words::join(command)),
        _ => bail!("the selected context has an invalid transport"),
      }
    }
  };
  let code = match args.code_file {
    Some(path) if path == std::path::Path::new("-") => {
      use std::io::Read as _;
      let mut code = String::new();
      std::io::stdin().read_to_string(&mut code).context("cannot read joining token from stdin")?;
      code
    }
    Some(path) => crate::configuration::read_secret(&path)?,
    None => {
      output::stderr_ln(format_args!("{}", joining_guidance(args.roles.as_deref())));
      rpassword::prompt_password("Joining token: ")?
    }
  };
  let code = code.trim();
  anyhow::ensure!(!code.is_empty() && !code.contains(['\r', '\n', '\0']), "provide one nonempty joining token");
  super::daemon::join_runner(
    super::daemon::RunnerArgs {
      name: Some(args.name),
      address: None,
      actor_image: args.actor_image,
      guest_needy: None,
      kvm: args.kvm,
      work_dir: None,
      gateway: None,
      gateway_upstream: None,
      gateway_upstream_credentials_file: None,
      ephemeral: false,
      join: Some(address),
      join_token_env: None,
      join_token_file: None,
      credential_env: None,
      credential_file: None,
      roles: args.roles,
      labels: args.labels,
      disconnect_grace: None,
      activity_hook: None,
      activity_hook_interval: None,
    },
    ctx,
    code.into(),
  )
  .await
}

/// Instruction text for the interactive prompt; explicit input skips it.
fn joining_guidance(roles: Option<&[RunnerRole]>) -> String {
  let roles = role_arguments(roles.unwrap_or_default());
  format!(
    "To obtain a joining token, run on the control host:\n\n  needy runner invite --ttl 1h{roles}\n\nCopy its token and paste it below.\nFor automation, use --code-file FILE or pipe the token with --code-file -.\n"
  )
}

fn role_arguments(roles: &[RunnerRole]) -> String {
  if roles.is_empty() {
    String::new()
  } else {
    format!(" --roles {}", roles.iter().map(ToString::to_string).collect::<Vec<_>>().join(","))
  }
}

async fn management(command: Command, ctx: &Context) -> Result<i32> {
  let fail = needy_api_client::Error::from;
  let human = !matches!(
    &command,
    Command::Invite(InviteArgs {
      output: OutputFormat::Json,
      ..
    }) | Command::Token {
      command: TokenCommand::Ls {
        output: OutputFormat::Json
      },
      ..
    } | Command::Ls {
      output: OutputFormat::Json,
      ..
    }
  );
  let progress = output::Progress::new(ctx.connection_progress(human), "Connecting to control service");
  let connection = ctx.connect().await?;
  let mut runners = connection.runners();
  match command {
    Command::Invite(InviteArgs {
      ttl,
      uses,
      roles: selected_roles,
      labels,
      output: format,
      connection: _,
    }) => {
      let request = CreateJoinTokenRequest {
        ttl_ms: i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX),
        uses,
        roles: selected_roles.into_iter().map(proto_role).collect(),
        labels: labels.into_iter().collect(),
      };
      progress.message("Creating runner invitation");
      let created = runners.create_join_token(request).await.map_err(fail)?.into_inner();
      progress.finish();
      let token = created.join_token.context("server returned an invitation without metadata")?;
      let roles = roles(&token.roles)?;
      match format {
        OutputFormat::Json => outln!(
          "{}",
          serde_json::to_string_pretty(
            &serde_json::json!({"version":1,"id":token.id,"roles":roles,"expires_at_ms":token.expires_at_ms,"uses_left":token.uses_left,"code":created.token})
          )?
        )?,
        OutputFormat::Table => {
          let selected_roles = token.roles.iter().map(|value| role(*value)).collect::<Result<Vec<_>>>()?;
          let arguments = role_arguments(&selected_roles);
          let roles = if roles.is_empty() {
            "any".to_string()
          } else {
            roles.join(",")
          };
          outln!(
            "Runner invitation created.\n\n  {:<12}{}\n  {:<12}{}\n  {:<12}{}\n  {:<12}{}\n\nOn the runner machine, run:\n\n  needy runner join <runner-name> --address <control-address>{arguments}\n\nUse an address reachable from the runner, or its local tunnel address.\nPaste the token when prompted.\n\nJoining token (shown once):\n\n  {}\n\nFor automation, create an invitation with -o json, extract .code,\nand pipe it to runner join with --code-file -. A private --code-file FILE also works.",
            "ID:",
            token.id,
            "Roles:",
            roles,
            "Expires:",
            time(token.expires_at_ms),
            "Uses left:",
            token.uses_left,
            created.token
          )?;
        }
      }
    }
    Command::Token {
      command: TokenCommand::Ls { output: format },
      ..
    } => {
      progress.message("Fetching runner invitations");
      let list = runners.list_join_tokens(ListJoinTokensRequest {}).await.map_err(fail)?.into_inner();
      progress.finish();
      let rows = list.join_tokens.into_iter().map(TokenRow::try_from).collect::<Result<Vec<_>>>()?;
      output::print(format, &rows)?;
    }
    Command::Token {
      command: TokenCommand::Revoke { id },
      ..
    } => {
      progress.message(format!("Revoking runner invitation {id}"));
      runners.revoke_join_token(RevokeJoinTokenRequest { id: id.clone() }).await.map_err(fail)?;
      progress.finish();
      output::acknowledge(&format!("Revoked runner invitation: {id}"))?;
    }
    Command::Ls { output: format, .. } => {
      progress.message("Fetching runners");
      let list = runners.list_runners(ListRunnersRequest {}).await.map_err(fail)?.into_inner();
      progress.finish();
      let rows = list.runners.into_iter().map(RunnerRow::try_from).collect::<Result<Vec<_>>>()?;
      output::print(format, &rows)?;
    }
    Command::Rotate { name, .. } => {
      progress.message(format!("Requesting credential rotation for runner {name}"));
      runners.rotate_runner(RotateRunnerRequest { name: name.clone() }).await.map_err(fail)?;
      progress.finish();
      output::acknowledge(&format!("Requested credential rotation for runner: {name}"))?;
    }
    Command::Rm { name, .. } => {
      progress.message(format!("Revoking runner {name}"));
      runners.remove_runner(RemoveRunnerRequest { name: name.clone() }).await.map_err(fail)?;
      progress.finish();
      output::acknowledge(&format!("Revoked runner: {name}"))?;
    }
    Command::Join(_) | Command::Serve(_) => unreachable!("local runner commands are dispatched before connecting"),
  }
  Ok(0)
}

#[cfg(test)]
#[path = "runner_test.rs"]
mod tests;
