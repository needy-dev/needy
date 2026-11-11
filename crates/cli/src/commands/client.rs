//! `needy client`: pairing this machine with a control plane, and the
//! clients that paired.
//!
//! An operator runs `needy client pair create` where it can reach the
//! control plane with the control token (a paired client may not) and
//! hands the code to the new client, which runs `needy client pair
//! --endpoint URL CODE`. The client's token is stored for that endpoint
//! (`crate::credentials`) and presented by every later command that reaches
//! it, so the trace names the client (`client:NAME`) as who did what.

use std::time::Duration;

use anyhow::Result;
use clap::Subcommand;
use needy_api_client::proto::v1::{self, CreatePairingCodeRequest, ListClientsRequest, PairRequest, RevokeClientRequest};
use serde::Serialize;

use super::runner::{ago, time};
use crate::cli::Context;
use crate::credentials::{self, Credential};
use crate::output::{self, OutputFormat, Row};

#[derive(clap::Args)]
pub struct Args {
  #[command(subcommand)]
  command: Command,
}

#[derive(Subcommand)]
enum Command {
  /// Pair this machine with the control plane at `--endpoint` using a
  /// pairing code, and store its client token for that endpoint.
  Pair(PairArgs),
  /// List paired clients, including revoked ones.
  Ls {
    #[arg(short = 'o', long, value_enum, default_value_t)]
    output: OutputFormat,
  },
  /// Revoke a client's token. It stays listed as revoked. Needs the
  /// control token.
  Revoke { id: String },
}

#[derive(clap::Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
struct PairArgs {
  #[command(subcommand)]
  command: Option<PairCommand>,
  /// The pairing code an operator created with `needy client pair create`.
  #[arg(required = true)]
  code: Option<String>,
}

#[derive(Subcommand)]
enum PairCommand {
  /// Create a pairing code for one client and print it. It is not shown
  /// again. Needs the control token; a paired client may not.
  Create {
    /// How long the code can be used to pair.
    #[arg(long, default_value = "10m", value_parser = humantime::parse_duration)]
    ttl: Duration,
    /// The client's name, which the trace records as `client:NAME`: a DNS
    /// label. Defaults to `client-<code ID>`.
    #[arg(long, default_value = "", hide_default_value = true)]
    name: String,
  },
}

#[derive(Serialize)]
struct ClientRow {
  id: String,
  name: String,
  /// `active` or `revoked`.
  state: String,
  created_at_ms: i64,
  /// Absent before the client's first request.
  last_used_at_ms: Option<i64>,
  revoked_at_ms: Option<i64>,
}

impl Row for ClientRow {
  const HEADER: &'static [&'static str] = &["ID", "NAME", "STATE", "PAIRED", "LAST USED"];

  fn cells(&self) -> Vec<String> {
    vec![
      self.id.clone(),
      self.name.clone(),
      self.state.clone(),
      time(self.created_at_ms),
      self.last_used_at_ms.map_or("-".into(), ago),
    ]
  }
}

impl From<v1::Client> for ClientRow {
  fn from(client: v1::Client) -> Self {
    let set = |ms: i64| (ms != 0).then_some(ms);
    Self {
      state: if client.revoked_at_ms != 0 {
        "revoked"
      } else {
        "active"
      }
      .into(),
      id: client.id,
      name: client.name,
      created_at_ms: client.created_at_ms,
      last_used_at_ms: set(client.last_used_at_ms),
      revoked_at_ms: set(client.revoked_at_ms),
    }
  }
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  let endpoint = ctx.endpoint()?;
  let fail = needy_api_client::Error::from;
  match args.command {
    Command::Pair(PairArgs {
      command: Some(PairCommand::Create { ttl, name }),
      ..
    }) => {
      let connection = crate::cli::connect(&endpoint).await?;
      let request = CreatePairingCodeRequest {
        ttl_ms: i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX),
        name,
      };
      let created = connection.clients().create_pairing_code(request).await.map_err(fail)?.into_inner();
      output::message(format_args!(
        "pairing code {} for client {}, valid until {}; it is not shown again",
        created.id,
        created.name,
        time(created.expires_at_ms)
      ));
      println!("{}", created.code);
    }
    Command::Pair(PairArgs {
      command: None,
      code: Some(code),
    }) => {
      // The pairing code authenticates the request; a stored or
      // environment token is not needed and is not sent.
      let connection = crate::cli::connect_without_token(&endpoint).await?;
      let paired = connection.clients().pair(PairRequest { code }).await.map_err(fail)?.into_inner();
      let client = paired.client.unwrap_or_default();
      let path = ctx.paths()?.credentials();
      let credential = Credential {
        client_id: client.id.clone(),
        name: client.name.clone(),
        token: paired.token,
        paired_at_ms: client.created_at_ms,
      };
      credentials::store(&path, &endpoint, credential)?;
      output::message(format_args!("paired as client {} ({}); its token for {endpoint} is in {}", client.name, client.id, path.display()));
    }
    Command::Pair(PairArgs {
      command: None,
      code: None,
    }) => unreachable!("clap requires a code or a subcommand"),
    Command::Ls { output: format } => {
      let connection = crate::cli::connect(&endpoint).await?;
      let list = connection.clients().list_clients(ListClientsRequest {}).await.map_err(fail)?.into_inner();
      let rows: Vec<ClientRow> = list.clients.into_iter().map(ClientRow::from).collect();
      output::print(format, &rows)?;
    }
    Command::Revoke { id } => {
      let connection = crate::cli::connect(&endpoint).await?;
      connection.clients().revoke_client(RevokeClientRequest { id }).await.map_err(fail)?;
    }
  }
  Ok(0)
}
