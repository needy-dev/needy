//! Global settings, saved transports and local profile files.
use crate::cli::Context;
use crate::configuration::{self, Locked};
use crate::output::{self, OutputFormat, Row, outln};
use anyhow::{Context as _, Result};
use clap::Subcommand;
use needy_api_client::{Client, proto::v1};
use needy_config::global::{self, Access, Config, Identity};
use serde::Serialize;
use std::path::PathBuf;
use std::time::Duration;

#[derive(clap::Args)]
pub struct Args {
  #[command(subcommand)]
  command: Command,
}
impl Args {
  pub fn allows_selection(&self) -> bool {
    matches!(
      &self.command,
      Command::Context {
        command: ContextCommand::Check { .. } | ContextCommand::Pair { .. } | ContextCommand::Set { .. }
      }
    ) || matches!(&self.command, Command::Profile(args) if args.uses_control())
  }
}
#[derive(Subcommand)]
enum Command {
  /// List global settings, saved contexts and local profiles, without a server.
  Ls {
    #[arg(short = 'o', long, value_enum, default_value_t)]
    output: OutputFormat,
  },
  /// Show global settings with credentials redacted.
  Show {
    #[arg(short = 'o', long, value_enum, default_value_t)]
    output: DocumentFormat,
  },
  /// Read a schema-defined global key.
  Get {
    key: String,
    #[arg(short = 'o', long, value_enum, default_value_t)]
    output: DocumentFormat,
  },
  /// Parse one typed YAML value and atomically save a valid complete candidate.
  Set { key: String, value: String },
  /// Restore a writable key's default.
  Unset { key: String },
  /// Show resolved directories and their override sources.
  Paths {
    #[arg(short = 'o', long, value_enum, default_value_t)]
    output: OutputFormat,
  },
  /// Select a saved connection or `local`, the built-in target for this machine's daemon.
  /// No server connection is required to save the selection.
  Use { name: String },
  /// Manage saved transports and identities.
  Context {
    #[command(subcommand)]
    command: ContextCommand,
  },
  /// Inspect local profile files. Environment values and credentials are masked.
  Profile(super::profile::Args),
}
#[derive(Clone, Copy, Default, clap::ValueEnum)]
pub enum DocumentFormat {
  #[default]
  Yaml,
  Json,
}

#[derive(clap::Args)]
struct Transport {
  // The root --address flag works before or after the subcommand. A local
  // flag with the same name shadows it during clap's required-arg checks.
  #[arg(skip)]
  address: Option<String>,
  /// Connector argv; stdout must carry protocol data only.
  #[arg(last = true, num_args = 1.., conflicts_with = "address")]
  command: Vec<String>,
}
impl Transport {
  fn validate(&self) -> Result<()> {
    match (&self.address, self.command.is_empty()) {
      (Some(address), true) => global::validate_address(address, false).map_err(Into::into),
      (None, false) if !self.command[0].is_empty() && self.command.iter().all(|s| !s.contains('\0')) => Ok(()),
      _ => anyhow::bail!("supply exactly one --address or connector argv after --"),
    }
  }
  async fn connect(&self) -> Result<Client> {
    self.validate()?;
    Ok(if let Some(address) = &self.address {
      Client::connect(address).await?
    } else {
      Client::connect_command(self.command.clone()).await?
    })
  }
}
#[derive(Subcommand)]
enum ContextCommand {
  Ls {
    #[arg(short = 'o', long, value_enum, default_value_t)]
    output: OutputFormat,
  },
  Show {
    name: String,
    #[arg(short = 'o', long, value_enum, default_value_t)]
    output: DocumentFormat,
  },
  /// Consume one invitation and save its independent identity; selection stays unchanged.
  Pair {
    name: String,
    #[command(flatten)]
    transport: Transport,
    /// Read an invitation from a private file, or `-` for standard input.
    #[arg(long)]
    code_file: Option<PathBuf>,
    #[arg(long)]
    replace: bool,
  },
  /// Replace transport only; keep identity and selection.
  Set {
    name: String,
    #[command(flatten)]
    transport: Transport,
  },
  /// Check health and authenticated identity without creating a run.
  Check {
    name: Option<String>,
    #[arg(long, default_value = "5s", value_parser = humantime::parse_duration)]
    timeout: Duration,
    #[arg(short = 'o', long, value_enum, default_value_t)]
    output: OutputFormat,
  },
  /// Remove a local record; revocation is a separate server operation.
  Rm { name: String },
}
#[derive(Serialize)]
struct Entry {
  kind: String,
  name: String,
  source: String,
  finding: String,
}
impl Row for Entry {
  const HEADER: &'static [&'static str] = &["KIND", "NAME", "SOURCE", "FINDING"];
  fn cells(&self) -> Vec<String> {
    vec![
      self.kind.clone(),
      self.name.clone(),
      self.source.clone(),
      self.finding.clone(),
    ]
  }
}
#[derive(Serialize)]
struct ContextRow {
  name: String,
  current: bool,
  transport: String,
  identity: String,
  recorded_access: String,
}

/// Health and authenticated identity are separate observations, not findings
/// about a configuration file. Missing values stay unknown on a failed probe.
#[derive(Serialize)]
struct CheckRow {
  name: String,
  address: String,
  status: &'static str,
  role: Option<String>,
  instance: Option<String>,
  client: Option<String>,
  client_id: Option<String>,
  access: Option<String>,
  error: Option<String>,
}

impl Row for CheckRow {
  const HEADER: &'static [&'static str] = &[
    "NAME", "ADDRESS", "STATUS", "ROLE", "INSTANCE", "CLIENT", "ACCESS",
  ];

  fn cells(&self) -> Vec<String> {
    let known = |value: &Option<String>| value.clone().unwrap_or_else(|| "unknown".into());
    vec![
      self.name.clone(),
      self.address.clone(),
      self.status.into(),
      known(&self.role),
      known(&self.instance),
      known(&self.client),
      known(&self.access),
    ]
  }
}
impl Row for ContextRow {
  const HEADER: &'static [&'static str] = &[
    "NAME",
    "CURRENT",
    "TRANSPORT",
    "RECORDED IDENTITY",
    "RECORDED ACCESS",
  ];
  fn cells(&self) -> Vec<String> {
    vec![
      self.name.clone(),
      self.current.to_string(),
      self.transport.clone(),
      self.identity.clone(),
      self.recorded_access.clone(),
    ]
  }
}

const WRITABLE: &[&str] = &[
  "paths.state",
  "paths.data",
  "paths.runtime",
  "daemon.listen",
  "daemon.actor_image",
  "daemon.local_runner",
  "daemon.guest_needy",
  "daemon.executor_image",
  "daemon.heartbeat_interval",
  "daemon.engine_host",
  "daemon.process_path",
];
fn key_parts(key: &str) -> Result<Vec<&str>> {
  let parts: Vec<_> = key.split('.').collect();
  if !(matches!(key, "version" | "current_context" | "contexts" | "paths" | "daemon") || WRITABLE.contains(&key)) {
    return Err(
      crate::diagnostics::Error::new("config.key_unknown", "Unknown global configuration key")
        .fact("field", key)
        .check(format!("Valid writable keys: {}", WRITABLE.join(", ")))
        .into(),
    );
  }
  Ok(parts)
}
fn document(value: &impl Serialize, format: DocumentFormat, secrets: &[&str]) -> Result<()> {
  let mut value = serde_json::to_value(value)?;
  configuration::redact(&mut value, secrets);
  match format {
    DocumentFormat::Json => outln!("{}", serde_json::to_string_pretty(&value)?)?,
    DocumentFormat::Yaml => output::write(format_args!("{}", crate::configuration::yaml(&value)?))?,
  }
  Ok(())
}
fn mutation(config: &Config, key: &str, value: Option<&str>) -> Result<Config> {
  if !WRITABLE.contains(&key) {
    return Err(
      crate::diagnostics::Error::new(
        "config.key_managed",
        "This key is managed or unknown; selection and credentials have dedicated commands",
      )
      .fact("field", key)
      .check(format!("Use config use/context; valid writable keys: {}", WRITABLE.join(", ")))
      .into(),
    );
  }
  let invalid = |cause: anyhow::Error| {
    crate::diagnostics::Error::new("config.invalid_field", "The value does not fit the global configuration field")
      .fact("field", key)
      .check("Use the field's documented type; the existing configuration has been preserved.")
      .cause(cause)
  };
  let mut candidate = serde_yaml::to_value(config)?;
  let parts = key_parts(key)?;
  let replacement = match value {
    Some(value) => serde_yaml::from_str(value).map_err(|cause| invalid(cause.into()))?,
    None => {
      let defaults = serde_yaml::to_value(Config::default())?;
      defaults[parts[0]][parts[1]].clone()
    }
  };
  candidate[parts[0]][parts[1]] = replacement;
  let candidate: Config = serde_yaml::from_value(candidate).map_err(|cause| invalid(cause.into()))?;
  candidate.validate().map_err(|cause| invalid(cause.into()))?;
  Ok(candidate)
}
fn context_rows(config: &Config) -> Vec<ContextRow> {
  let secrets: Vec<_> = config.contexts.values().map(|r| r.credential.as_str()).collect();
  let mut rows = vec![ContextRow {
    name: "local".into(),
    current: config.current_context == "local",
    transport: "local discovery".into(),
    identity: "OS account".into(),
    recorded_access: "unknown until checked".into(),
  }];
  rows.extend(config.contexts.iter().map(|(name, entry)| ContextRow {
    name: name.clone(),
    current: config.current_context == *name,
    transport: configuration::safe_text(
      &entry.address.clone().unwrap_or_else(|| shell_words::join(entry.command.as_ref().unwrap())),
      &secrets,
    ),
    identity: format!("{} ({})", entry.identity.name, entry.identity.client_id),
    recorded_access: format!("{:?}", entry.identity.access).to_lowercase(),
  }));
  rows
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  let path = ctx.configuration_file()?;
  match args.command {
    Command::Profile(args) => return super::profile::run(args, ctx).await,
    Command::Set { key, value } => {
      let mut locked = Locked::open(&path)?;
      locked.config = mutation(&locked.config, &key, Some(&value))?;
      locked.save()?;
    }
    Command::Unset { key } => {
      let mut locked = Locked::open(&path)?;
      locked.config = mutation(&locked.config, &key, None)?;
      locked.save()?;
    }
    Command::Use { name } => {
      let mut locked = Locked::open(&path)?;
      if name != "local" && !locked.config.contexts.contains_key(&name) {
        return Err(missing(&name));
      }
      locked.config.current_context = name;
      locked.save()?;
    }
    Command::Context { command } => return context(command, ctx, &path).await,
    Command::Show { output } => {
      let config = ctx.configuration()?;
      let secrets: Vec<_> = config.contexts.values().map(|r| r.credential.as_str()).collect();
      document(&config, output, &secrets)?;
    }
    Command::Get { key, output } => {
      let parts = key_parts(&key)?;
      let config = ctx.configuration()?;
      let secrets: Vec<_> = config.contexts.values().map(|r| r.credential.as_str()).collect();
      let value = serde_json::to_value(&config)?;
      let mut selected = &value;
      for part in parts {
        selected = &selected[part];
      }
      document(&selected, output, &secrets)?;
    }
    Command::Ls { output } => {
      let config = ctx.configuration()?;
      let mut rows = vec![Entry {
        kind: "global".into(),
        name: "configuration".into(),
        source: path.display().to_string(),
        finding: String::new(),
      }];
      rows.extend(context_rows(&config).into_iter().map(|row| Entry {
        kind: "context".into(),
        name: row.name,
        source: path.display().to_string(),
        finding: String::new(),
      }));
      rows.extend(super::profile::entries(ctx)?.into_iter().map(|(name, source, finding)| Entry {
        kind: "profile".into(),
        name,
        source: source.display().to_string(),
        finding,
      }));
      output::print(output, &rows)?;
    }
    Command::Paths { output } => {
      let paths = ctx.paths()?;
      let saved = ctx.configuration()?;
      let rows: Vec<_> = [
        ("config_file", paths.config_file(), "NEEDY_CONFIG_DIR", false),
        ("config", paths.config.clone(), "NEEDY_CONFIG_DIR", false),
        ("profiles", paths.profiles(), "NEEDY_CONFIG_DIR", false),
        ("state", paths.state.clone(), "NEEDY_STATE_DIR", saved.paths.state.is_some()),
        ("data", paths.data.clone(), "NEEDY_DATA_DIR", saved.paths.data.is_some()),
        ("runtime", paths.runtime.clone(), "NEEDY_RUNTIME_DIR", saved.paths.runtime.is_some()),
      ]
      .into_iter()
      .map(|(name, path, env, saved)| Entry {
        kind: "path".into(),
        name: name.into(),
        source: path.display().to_string(),
        finding: if matches!(name, "config_file" | "config" | "profiles") && ctx.config.is_some() {
          "--config".into()
        } else if std::env::var_os(env).is_some_and(|s| !s.is_empty()) {
          env.into()
        } else if saved {
          format!("{}:paths.{name}", paths.config_file().display())
        } else {
          "platform default".into()
        },
      })
      .collect();
      output::print(output, &rows)?;
    }
  }
  Ok(0)
}

async fn context(command: ContextCommand, ctx: &Context, path: &std::path::Path) -> Result<i32> {
  match command {
    ContextCommand::Ls { output } => output::print(output, &context_rows(&ctx.configuration()?))?,
    ContextCommand::Show { name, output } => {
      let config = ctx.configuration()?;
      if name == "local" {
        document(&serde_json::json!({"name":"local", "transport":"local discovery", "identity":"OS account"}), output, &[])?;
      } else {
        let record = config.contexts.get(&name).ok_or_else(|| missing(&name))?;
        document(record, output, &[&record.credential])?;
      }
    }
    ContextCommand::Rm { name } => {
      let mut locked = Locked::open(path)?;
      anyhow::ensure!(name != "local", "built-in local cannot be removed");
      if locked.config.current_context == name {
        return Err(
          crate::diagnostics::Error::new("config.context_conflict", "Switch with config use before removing the current context")
            .fact("context", &name)
            .check("Select local or another saved connection, then remove this local record.")
            .into(),
        );
      }
      if locked.config.contexts.remove(&name).is_none() {
        return Err(missing(&name));
      }
      locked.save()?;
    }
    ContextCommand::Set {
      name,
      mut transport,
    } => {
      anyhow::ensure!(
        ctx.selected_context.is_none() && ctx.credential_file.is_none(),
        "context set changes its named transport; omit --context and --credential-file"
      );
      transport.address = ctx.address.clone();
      transport.validate()?;
      let mut locked = Locked::open(path)?;
      let record = locked.config.contexts.get_mut(&name).ok_or_else(|| missing(&name))?;
      record.address = transport.address;
      record.command = (!transport.command.is_empty()).then_some(transport.command);
      locked.save()?;
    }
    ContextCommand::Pair {
      name,
      mut transport,
      code_file,
      replace,
    } => {
      anyhow::ensure!(
        ctx.selected_context.is_none() && ctx.credential_file.is_none(),
        "pair uses its own transport and invitation; omit --context and --credential-file"
      );
      transport.address = ctx.address.clone();
      transport.validate()?;
      anyhow::ensure!(name != "local" && global::valid_name(&name), "context name requires a DNS label; local is reserved");
      let mut locked = Locked::open(path)?;
      let exists = locked.config.contexts.contains_key(&name);
      if exists != replace {
        return Err(
          crate::diagnostics::Error::new(
            "config.context_conflict",
            "An existing context requires --replace; --replace requires an existing context",
          )
          .fact("context", &name)
          .check("Inspect config context ls before consuming an invitation.")
          .into(),
        );
      }
      let code = match code_file {
        Some(path) if path == std::path::Path::new("-") => {
          use std::io::Read as _;
          let mut code = String::new();
          std::io::stdin().read_to_string(&mut code).context("cannot read client invitation from stdin")?;
          code
        }
        Some(path) => configuration::read_secret(&path).context("cannot read invitation code file")?,
        None => {
          eprintln!(
            "To obtain a client invitation, run on the control host:\n\n  needy client invite --name <client-name>\n\nPaste its code below. For automation, use --code-file FILE\nor pipe the code with --code-file -.\n"
          );
          rpassword::prompt_password("Invitation code: ")
            .context("cannot read hidden invitation code; use --code-file FILE or --code-file -")?
        }
      };
      let code = code.trim();
      anyhow::ensure!(!code.is_empty() && !code.contains(['\r', '\n', '\0']), "provide one nonempty client invitation code");
      let connection = transport.connect().await?;
      let paired =
        connection.clients().pair(v1::PairRequest { code: code.into() }).await.map_err(needy_api_client::Error::from)?.into_inner();
      let client = paired.client.ok_or_else(|| unsaved("unknown", anyhow::anyhow!("server issued a credential without client identity")))?;
      let access = match v1::ClientAccess::try_from(client.access) {
        Ok(v1::ClientAccess::Runs) => Access::Runs,
        Ok(v1::ClientAccess::Admin) => Access::Admin,
        _ => return Err(unsaved(&client.id, anyhow::anyhow!("server issued an unknown client access value"))),
      };
      let id = client.id.clone();
      let record = global::Context {
        address: transport.address,
        command: (!transport.command.is_empty()).then_some(transport.command),
        credential: paired.token,
        identity: Identity {
          client_id: client.id,
          name: client.name,
          access,
          paired_at_ms: client.created_at_ms,
        },
      };
      record.validate().map_err(|cause| unsaved(&id, cause.into()))?;
      connection.with_token(&record.credential).map_err(|cause| unsaved(&id, cause.into()))?;
      locked.config.contexts.insert(name.clone(), record);
      locked.save().map_err(|cause| unsaved(&id, cause))?;
      output::success("Paired successfully!")?;
      let local_command = match &ctx.config {
        Some(path) => shell_words::join(["needy", "--config", &path.to_string_lossy()]),
        None => "needy".into(),
      };
      outln!(
        "\nSaved context: {name}\nClient ID: {id}\nCurrent context: {}\n\nTo use this connection by default:\n\n  {local_command} config use {name}\n\nTo check this connection:\n\n  {local_command} config context check {name}",
        locked.config.current_context
      )?;
    }
    ContextCommand::Check {
      name,
      timeout,
      output,
    } => {
      let config = ctx.configuration()?;
      anyhow::ensure!(
        name.is_none() || (ctx.selected_context.is_none() && ctx.address.is_none()),
        "a context name conflicts with global --context/--address"
      );
      let direct = ctx.address.is_some();
      let name = if direct {
        "direct".into()
      } else {
        name.or_else(|| ctx.selected_context.clone()).unwrap_or(config.current_context.clone())
      };
      let record = if direct {
        None
      } else {
        config.contexts.get(&name)
      };
      if !direct && name != "local" && record.is_none() {
        return Err(missing(&name));
      }
      let transport = ctx.address.clone().unwrap_or_else(|| {
        record.map(|r| r.address.clone().unwrap_or_else(|| "connector".into())).unwrap_or_else(|| "local discovery".into())
      });
      let probe = async {
        let client = if direct {
          ctx.connect().await?
        } else if let Some(record) = record {
          Transport {
            address: record.address.clone(),
            command: record.command.clone().unwrap_or_default(),
          }
          .connect()
          .await?
          .with_token(&record.credential)?
        } else {
          crate::connection::local(ctx).await?
        };
        let health = client.health().check(v1::CheckRequest {}).await.map_err(needy_api_client::Error::from)?.into_inner();
        anyhow::ensure!(
          matches!(health.role.as_str(), "control" | "executor") && !health.instance_id.is_empty(),
          "endpoint did not identify a supported service role and instance"
        );
        let (identity, client_id, access) = if health.role == "executor" {
          (None, None, None)
        } else {
          match client.clients().get_caller(v1::GetCallerRequest {}).await {
            Ok(response) => {
              let c = response.into_inner();
              let access = match v1::ClientAccess::try_from(c.access) {
                Ok(v1::ClientAccess::Admin) => "admin",
                Ok(v1::ClientAccess::Runs) => "runs",
                _ => "unknown",
              };
              (Some(c.name), Some(c.client_id), Some(access.to_owned()))
            }
            Err(e) if e.code() == tonic::Code::Unimplemented => (None, None, None),
            Err(e) => return Err(anyhow::Error::from(needy_api_client::Error::from(e))),
          }
        };
        Ok::<_, anyhow::Error>((health.role, health.instance_id, identity, client_id, access))
      };
      let result = tokio::time::timeout(timeout, probe).await;
      let secrets: Vec<_> = config.contexts.values().map(|r| r.credential.as_str()).collect();
      let mut row = CheckRow {
        name,
        address: configuration::safe_text(&transport, &secrets),
        status: "error",
        role: None,
        instance: None,
        client: None,
        client_id: None,
        access: None,
        error: None,
      };
      let failure = match result {
        Ok(Ok((role, instance, client, client_id, access))) => {
          row.status = "ok";
          row.role = Some(configuration::safe_text(&role, &secrets));
          row.instance = Some(configuration::safe_text(&instance, &secrets));
          row.client = client.map(|v| configuration::safe_text(&v, &secrets));
          row.client_id = client_id.map(|v| configuration::safe_text(&v, &secrets));
          row.access = access;
          None
        }
        Ok(Err(error)) => Some(error),
        Err(_) => Some(anyhow::anyhow!("connection check timed out after {}", humantime::format_duration(timeout))),
      };
      row.error = failure.as_ref().map(|e| configuration::safe_text(&format!("{e:#}"), &secrets));
      // A positional check target can differ from the saved default. Error
      // advice must retain the target that was actually probed.
      let checked_context = Context {
        config: ctx.config.clone(),
        selected_context: (!direct).then(|| row.name.clone()),
        address: ctx.address.clone(),
        credential_file: ctx.credential_file.clone(),
        diagnostic_format: ctx.diagnostic_format,
      };
      output::print(output, &[row])?;
      if let Some(error) = failure {
        crate::diagnostics::render_in(&error, ctx.diagnostic_format, Some(&checked_context));
        return Ok(1);
      }
      return Ok(0);
    }
  }
  Ok(0)
}

fn missing(name: &str) -> anyhow::Error {
  crate::diagnostics::Error::new("config.context_not_found", "The saved context does not exist")
    .fact("context", name)
    .check("Inspect config context ls, then select an existing name or pair a new one.")
    .into()
}

fn unsaved(id: &str, cause: anyhow::Error) -> anyhow::Error {
  crate::diagnostics::Error::new("bootstrap.save_failed", "The credential was issued but not saved; the old local record remains")
    .fact("client_id", id)
    .check("Revoke this public client ID and request a fresh invitation; do not retry the consumed code.")
    .cause(cause)
    .into()
}
