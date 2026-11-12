//! `needy pair`: pairing this machine with a control plane, and the
//! clients that paired.
//!
//! An operator runs `needy pair create` where it can reach the control
//! plane with the control token (a paired client may not) and hands the
//! code to the new client, which runs `needy pair --endpoint URL CODE`
//! once. The client's token is stored for that endpoint
//! (`crate::credentials`) and presented by every later command that reaches
//! it, so the trace names the client (`client:NAME`) as who did what; the
//! endpoint becomes the machine's default, so later commands need no
//! `--endpoint`.

use std::time::Duration;

use anyhow::Result;
use clap::Subcommand;
use needy_api_client::proto::v1::{self, CreatePairingCodeRequest, ListClientsRequest, PairRequest, RevokeClientRequest};
use serde::Serialize;

use super::runner::{ago, time};
use crate::cli::Context;
use crate::credentials::{self, Credential};
use crate::output::{self, OutputFormat, Row};

/// `needy pair CODE` pairs this machine; `needy pair create|ls|revoke`
/// manage pairing codes and paired clients.
#[derive(clap::Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub struct Args {
  #[command(subcommand)]
  command: Option<Command>,
  /// The pairing code an operator created with `needy pair create`. Pairs
  /// this machine with the control plane at `--endpoint` once, stores its
  /// client token for that endpoint, and makes that endpoint the default
  /// for later commands.
  #[arg(required = true)]
  code: Option<String>,
}

#[derive(Subcommand)]
enum Command {
  /// Create a pairing code for one client and print it. It is not shown
  /// again. Needs the control token; a paired client may not.
  Create {
    /// How long the code can be used to pair.
    #[arg(long, default_value = "10m", value_parser = humantime::parse_duration)]
    ttl: Duration,
    /// The client's name, which the trace records as `client:NAME`: a DNS
    /// label. Without it the code gets a fresh `client-<code ID>` that no
    /// client holds. The name of an active client re-pairs it: pairing with
    /// the code replaces that client and revokes its token, and the command
    /// says so.
    #[arg(long, default_value = "", hide_default_value = true)]
    name: String,
  },
  /// List paired clients, including revoked ones. Only the operator, on
  /// the control host's Unix socket.
  Ls {
    #[arg(short = 'o', long, value_enum, default_value_t)]
    output: OutputFormat,
  },
  /// Revoke a client's token. It stays listed as revoked. Needs the
  /// control token.
  Revoke { id: String },
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
  match (args.command, args.code) {
    (Some(Command::Create { ttl, name }), _) => {
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
      if let Some(old) = &created.replaces {
        output::message(format_args!("notice: pairing with this code will replace client {} ({}) and revoke its token", old.name, old.id));
      }
      println!("{}", created.code);
    }
    (None, Some(code)) => {
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
      if let Some(warning) = credentials::store(&path, &endpoint, credential)? {
        output::message(format_args!("warning: {warning}"));
      }
      output::message(format_args!(
        "paired as client {} ({}); its token for {endpoint} is in {}, and commands reach {endpoint} by default",
        client.name,
        client.id,
        path.display()
      ));
    }
    (None, None) => unreachable!("clap requires a code or a subcommand"),
    (Some(Command::Ls { output: format }), _) => {
      let connection = crate::cli::connect(&endpoint).await?;
      let list = connection.clients().list_clients(ListClientsRequest {}).await.map_err(fail)?.into_inner();
      let rows: Vec<ClientRow> = list.clients.into_iter().map(ClientRow::from).collect();
      output::print(format, &rows)?;
    }
    (Some(Command::Revoke { id }), _) => {
      let connection = crate::cli::connect(&endpoint).await?;
      connection.clients().revoke_client(RevokeClientRequest { id }).await.map_err(fail)?;
    }
  }
  Ok(0)
}
