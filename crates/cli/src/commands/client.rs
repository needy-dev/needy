//! Server-side client invitations and revocation. Local pairing belongs to config.

use crate::cli::{ConnectionArgs, Context};
use crate::output::{self, OutputFormat, Row, ago, outln, time};
use anyhow::{Result, bail};
use clap::Subcommand;
use needy_api_client::proto::v1::{self, ClientAccess, CreatePairingCodeRequest, GetCallerRequest, ListClientsRequest, RevokeClientRequest};
use serde::Serialize;
use std::time::Duration;

#[derive(clap::Args)]
pub struct Args {
  #[command(flatten)]
  pub(crate) connection: ConnectionArgs,
  #[command(subcommand)]
  command: Command,
}

#[derive(Subcommand)]
enum Command {
  /// Issue a one-time client invitation (the token is shown once).
  Invite {
    /// Time until the invitation expires (for example `10m` or `1h`).
    #[arg(long, default_value = "10m", value_parser = humantime::parse_duration)]
    ttl: Duration,
    /// Display name for the invited client.
    #[arg(long, default_value = "")]
    name: String,
    /// Grant administrator access (default invitations permit run and process use only).
    #[arg(long)]
    admin: bool,
    /// Output format.
    #[arg(short = 'o', long, value_enum, default_value_t)]
    output: OutputFormat,
  },
  /// List registered clients, including revoked ones.
  Ls {
    /// Output format.
    #[arg(short = 'o', long, value_enum, default_value_t)]
    output: OutputFormat,
  },
  /// Revoke a client's credential while keeping its audit record.
  Revoke {
    /// Client ID to revoke.
    client_id: String,
  },
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
  const HEADER: &'static [&'static str] = &["ID", "NAME", "CLIENT ROLE", "STATE", "PAIRED", "LAST USED"];

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
  let human = match &args.command {
    Command::Invite { output, .. } | Command::Ls { output } => matches!(output, OutputFormat::Table),
    Command::Revoke { .. } => true,
  };
  let progress = output::Progress::new(human && ctx.connection_feedback(), "Connecting to control service...");
  let connection = ctx.connect().await?;
  let fail = needy_api_client::Error::from;
  match args.command {
    Command::Invite {
      ttl,
      name,
      admin,
      output: format,
    } => {
      progress.message("Creating client invitation...");
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
      progress.finish();
      if let Some(old) = &created.replaces {
        output::message(format!("pairing with this invitation replaces client {} ({}) and revokes its credential", old.name, old.id));
      }
      let row = serde_json::json!({"version":1,"id":created.id,"name":created.name,"access":if admin {"admin"} else {"runs"},"expires_at_ms":created.expires_at_ms,"code":created.code});
      match format {
        OutputFormat::Json => outln!("{}", serde_json::to_string_pretty(&row)?)?,
        OutputFormat::Table => {
          output::success("Client invitation created.")?;
          outln!(
            "\n  {:<12}{}\n  {:<12}{}\n  {:<12}{}\n  {:<12}{}\n\nOn the client machine, run:\n\n  needy config context pair control --address <control-address> --token {}\n\nUse an address reachable from the client, or its local tunnel address.\nThen select and check the connection:\n\n  needy config use control\n  needy config context check control\n\nThe token is shown once. Omit --token to enter it at the hidden prompt.\nFor automation, use -o json, extract .code, and pipe it to pair with\n--code-file -. A private --code-file FILE also works.",
            "ID",
            created.id,
            "NAME",
            created.name,
            "CLIENT ROLE",
            if admin { "admin" } else { "runs" },
            "EXPIRES",
            time(created.expires_at_ms),
            shell_words::quote(&created.code)
          )?;
        }
      }
    }
    Command::Ls { output: format } => {
      progress.message("Fetching clients...");
      let list = connection.clients().list_clients(ListClientsRequest {}).await.map_err(fail)?.into_inner();
      progress.finish();
      let rows: Vec<ClientRow> = list.clients.into_iter().map(ClientRow::from).collect();
      output::print(format, &rows)?;
    }
    Command::Revoke { client_id } => {
      progress.message("Revoking client...");
      connection.clients().revoke_client(RevokeClientRequest { id: client_id }).await.map_err(fail)?;
      progress.finish();
      output::acknowledge("Client revoked.")?;
    }
  }
  Ok(0)
}
