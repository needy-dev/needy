//! Server-side client invitations and revocation. Local pairing belongs to config.

use super::runner::{ago, time};
use crate::cli::Context;
use crate::output::{self, OutputFormat, Row, outln};
use anyhow::{Result, bail};
use clap::Subcommand;
use needy_api_client::proto::v1::{self, ClientAccess, CreatePairingCodeRequest, GetCallerRequest, ListClientsRequest, RevokeClientRequest};
use serde::Serialize;
use std::time::Duration;

#[derive(clap::Args)]
pub struct Args {
  #[command(subcommand)]
  command: Command,
}

#[derive(Subcommand)]
enum Command {
  /// Issue a one-use client invitation. The secret is shown once.
  Invite {
    #[arg(long, default_value = "10m", value_parser = humantime::parse_duration)]
    ttl: Duration,
    #[arg(long, default_value = "")]
    name: String,
    /// Grant management access; omitted invitations permit run/process use.
    #[arg(long)]
    admin: bool,
    #[arg(short = 'o', long, value_enum, default_value_t)]
    output: OutputFormat,
  },
  /// List public identities, including revoked clients.
  Ls {
    #[arg(short = 'o', long, value_enum, default_value_t)]
    output: OutputFormat,
  },
  /// Revoke one identity while keeping its audit record.
  Revoke { client_id: String },
}

#[derive(Serialize)]
struct ClientRow {
  id: String,
  name: String,
  /// `active` or `revoked`.
  state: String,
  access: String,
  created_at_ms: i64,
  /// Absent before the client's first request.
  last_used_at_ms: Option<i64>,
  revoked_at_ms: Option<i64>,
}

impl Row for ClientRow {
  const HEADER: &'static [&'static str] = &["ID", "NAME", "ACCESS", "STATE", "PAIRED", "LAST USED"];

  fn cells(&self) -> Vec<String> {
    vec![
      self.id.clone(),
      self.name.clone(),
      self.access.clone(),
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
      access: match ClientAccess::try_from(client.access) {
        Ok(ClientAccess::Admin) => "admin",
        Ok(ClientAccess::Runs | ClientAccess::Unspecified) => "runs",
        Err(_) => "unknown",
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
  let connection = ctx.connect().await?;
  let fail = needy_api_client::Error::from;
  match args.command {
    Command::Invite {
      ttl,
      name,
      admin,
      output: format,
    } => {
      let access = if admin {
        ClientAccess::Admin
      } else {
        ClientAccess::Runs
      };
      if admin {
        let caller = connection.clients().get_caller(GetCallerRequest {}).await.map_err(fail)?.into_inner();
        if caller.access != ClientAccess::Admin as i32 {
          bail!("administrator invitations require local operator or verified administrator access; check `needy config context check`");
        }
      }
      let created = connection
        .clients()
        .create_pairing_code(CreatePairingCodeRequest {
          ttl_ms: i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX),
          name,
          access: access.into(),
        })
        .await
        .map_err(fail)?
        .into_inner();
      if admin && created.access != ClientAccess::Admin as i32 {
        bail!("the server did not confirm administrator access; no administrator invitation was accepted");
      }
      if let Some(old) = &created.replaces {
        output::message(format!("pairing with this invitation replaces client {} ({}) and revokes its credential", old.name, old.id));
      }
      let row = serde_json::json!({"version":1,"id":created.id,"name":created.name,"access":if admin {"admin"} else {"runs"},"expires_at_ms":created.expires_at_ms,"code":created.code});
      match format {
        OutputFormat::Json => outln!("{}", serde_json::to_string_pretty(&row)?)?,
        OutputFormat::Table => {
          outln!(
            "ID  {}\nNAME  {}\nACCESS  {}\nEXPIRES  {}\nCODE (shown once)  {}",
            created.id,
            created.name,
            if admin { "admin" } else { "runs" },
            time(created.expires_at_ms),
            created.code
          )?;
        }
      }
    }
    Command::Ls { output: format } => {
      let list = connection.clients().list_clients(ListClientsRequest {}).await.map_err(fail)?.into_inner();
      let rows: Vec<ClientRow> = list.clients.into_iter().map(ClientRow::from).collect();
      output::print(format, &rows)?;
    }
    Command::Revoke { client_id } => {
      connection.clients().revoke_client(RevokeClientRequest { id: client_id }).await.map_err(fail)?;
    }
  }
  Ok(0)
}
