//! `needy runner`: join tokens and the runners that joined with them.
//! A runner itself is `needy daemon --mode runner`.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use anyhow::{Result, bail};
use clap::Subcommand;
use needy_api_client::proto::v1::{
  self, CreateJoinTokenRequest, ListJoinTokensRequest, ListRunnersRequest, RemoveRunnerRequest, RevokeJoinTokenRequest, RotateRunnerRequest,
};
use needy_core::types::RunnerRole;
use serde::Serialize;

use crate::cli::Context;
use crate::output::{self, OutputFormat, Row};

#[derive(clap::Args)]
pub struct Args {
  #[command(subcommand)]
  command: Command,
}

#[derive(Subcommand)]
enum Command {
  /// Manage join tokens, which let a machine join as a runner.
  Token {
    #[command(subcommand)]
    command: TokenCommand,
  },
  /// List runners, including removed ones.
  Ls {
    #[arg(short = 'o', long, value_enum, default_value_t)]
    output: OutputFormat,
  },
  /// Give a runner a new credential at its next heartbeat. Its old one
  /// works until it uses the new one.
  Rotate { name: String },
  /// Revoke a runner's credential. It stays listed as removed.
  Rm { name: String },
}

#[derive(Subcommand)]
enum TokenCommand {
  /// Create a join token and print it. It is not shown again.
  Create {
    /// How long the token can be used to join.
    #[arg(long, default_value = "1h", value_parser = humantime::parse_duration)]
    ttl: Duration,
    /// How many runners can join with it.
    #[arg(long, default_value_t = 1)]
    uses: u32,
    /// The roles a runner may declare: actor, executor. Every role when
    /// none is given.
    #[arg(long, value_delimiter = ',')]
    roles: Vec<RunnerRole>,
    /// Labels every runner that joins with it gets: KEY=VALUE,...
    #[arg(long, value_delimiter = ',', value_parser = label)]
    labels: Vec<(String, String)>,
  },
  /// List join tokens, including expired, spent and revoked ones.
  Ls {
    #[arg(short = 'o', long, value_enum, default_value_t)]
    output: OutputFormat,
  },
  /// Revoke a join token. Runners that joined with it stay.
  Revoke { id: String },
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

pub(crate) fn time(ms: i64) -> String {
  let at = SystemTime::UNIX_EPOCH + Duration::from_millis(u64::try_from(ms).unwrap_or(0));
  humantime::format_rfc3339_seconds(at).to_string()
}

/// How long ago `ms` was on this machine's clock, to the second.
pub(crate) fn ago(ms: i64) -> String {
  let seconds = u64::try_from(needy_common::time::now_ms().saturating_sub(ms) / 1000).unwrap_or(0);
  format!("{} ago", humantime::format_duration(Duration::from_secs(seconds)))
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

#[derive(Serialize)]
struct RunnerRow {
  name: String,
  status: String,
  /// A new credential was asked for and the runner has not used it yet.
  rotating: bool,
  roles: Vec<String>,
  labels: BTreeMap<String, String>,
  created_at_ms: i64,
  last_seen_at_ms: i64,
  join_token_id: String,
}

impl Row for RunnerRow {
  const HEADER: &'static [&'static str] = &["NAME", "STATUS", "ROLES", "LABELS", "LAST SEEN"];

  fn cells(&self) -> Vec<String> {
    let status = if self.rotating {
      format!("{} (rotating)", self.status)
    } else {
      self.status.clone()
    };
    vec![
      self.name.clone(),
      status,
      self.roles.join(","),
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
      roles: roles(&runner.roles)?,
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
  let endpoint = ctx.endpoint()?;
  let fail = needy_api_client::Error::from;
  let connection = crate::cli::connect(&endpoint).await?;
  let mut runners = connection.runners();
  match args.command {
    Command::Token {
      command: TokenCommand::Create {
        ttl,
        uses,
        roles,
        labels,
      },
    } => {
      let request = CreateJoinTokenRequest {
        ttl_ms: i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX),
        uses,
        roles: roles.into_iter().map(proto_role).collect(),
        labels: labels.into_iter().collect(),
      };
      let created = runners.create_join_token(request).await.map_err(fail)?.into_inner();
      let id = created.join_token.map(|t| t.id).unwrap_or_default();
      output::message(format_args!("join token {id}; it is not shown again"));
      println!("{}", created.token);
    }
    Command::Token {
      command: TokenCommand::Ls { output: format },
    } => {
      let list = runners.list_join_tokens(ListJoinTokensRequest {}).await.map_err(fail)?.into_inner();
      let rows = list.join_tokens.into_iter().map(TokenRow::try_from).collect::<Result<Vec<_>>>()?;
      output::print(format, &rows)?;
    }
    Command::Token {
      command: TokenCommand::Revoke { id },
    } => {
      runners.revoke_join_token(RevokeJoinTokenRequest { id }).await.map_err(fail)?;
    }
    Command::Ls { output: format } => {
      let list = runners.list_runners(ListRunnersRequest {}).await.map_err(fail)?.into_inner();
      let rows = list.runners.into_iter().map(RunnerRow::try_from).collect::<Result<Vec<_>>>()?;
      output::print(format, &rows)?;
    }
    Command::Rotate { name } => {
      runners.rotate_runner(RotateRunnerRequest { name }).await.map_err(fail)?;
    }
    Command::Rm { name } => {
      runners.remove_runner(RemoveRunnerRequest { name }).await.map_err(fail)?;
    }
  }
  Ok(0)
}

#[cfg(test)]
#[path = "runner_test.rs"]
mod tests;
