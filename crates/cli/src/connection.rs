//! Resolve one target and one credential source before opening a transport.
//! A failed connection never selects another daemon or a stronger identity.

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use needy_api_client::Client;
use serde::{Deserialize, Serialize};

use crate::cli::Context;

/// Internal, non-secret execution ABI. Credentials are resolved on the machine
/// consuming this reference; they are never copied into the environment.
pub const EXECUTION_CONTEXT: &str = "NEEDY_EXECUTION_CONTEXT";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Execution {
  pub version: u32,
  pub config_file: PathBuf,
  pub context: Option<String>,
  pub address: Option<String>,
  pub command: Option<Vec<String>>,
  pub run: Option<String>,
}

pub fn inherited() -> Result<Option<Execution>> {
  let Some(value) = std::env::var_os(EXECUTION_CONTEXT).filter(|v| !v.is_empty()) else {
    return Ok(None);
  };
  let execution: Execution = serde_json::from_slice(value.as_encoded_bytes())
    .context("invalid inherited execution context; start a new session with an explicit --context or --address")?;
  if execution.version != 1 || !execution.config_file.is_absolute() {
    bail!("unsupported inherited execution context; select --context or --address explicitly");
  }
  Ok(Some(execution))
}

/// Resolved transport and credential. Deliberately has no Debug implementation.
pub struct Resolved {
  pub context: Option<String>,
  pub source: &'static str,
  pub address: Option<String>,
  pub command: Option<Vec<String>>,
  credential: Option<String>,
  config_file: Option<PathBuf>,
  expected_instance: Option<String>,
}

impl Resolved {
  pub fn display(&self) -> String {
    self
      .address
      .clone()
      .unwrap_or_else(|| format!("connector: {}", self.command.as_ref().and_then(|c| c.first()).map(String::as_str).unwrap_or("unknown")))
  }

  pub async fn connect(&self) -> Result<Client> {
    let client = match (&self.address, &self.command) {
      (Some(address), None) => Client::connect(address).await,
      (None, Some(command)) => Client::connect_command(command.clone()).await,
      _ => bail!("the selected connection has no valid transport"),
    }
    .map_err(|cause| {
      let diagnostic = match &cause {
        needy_api_client::Error::Connector {
          reason,
          program,
          exit_code,
          ..
        } => {
          let mut diagnostic = crate::diagnostics::Error::new(reason, "Cannot establish the connector transport").fact("program", program);
          if let Some(code) = exit_code {
            diagnostic = diagnostic.fact("exit_code", code.to_string());
          }
          diagnostic
        }
        _ => crate::diagnostics::Error::new("connection.unreachable", "Cannot reach the selected daemon"),
      };
      diagnostic
        .fact("source", self.source)
        .fact("transport", self.display())
        .check(if self.context.as_deref() == Some("local") {
          "Start the local control plane with `needy daemon serve` and inspect its console."
        } else {
          "Run `needy config context check` for the selected connection and verify its tunnel or connector."
        })
        .cause(cause)
    })?;
    if let Some(expected) = &self.expected_instance {
      let health =
        tokio::time::timeout(std::time::Duration::from_secs(5), client.health().check(needy_api_client::proto::v1::CheckRequest {}))
          .await
          .map_err(|_| {
            crate::diagnostics::Error::new("connection.timeout", "The local daemon did not answer its health check in time")
              .check("Inspect the local daemon console and discovery ownership before retrying.")
          })?
          .map_err(needy_api_client::Error::from)?
          .into_inner();
      if health.role != "control" || &health.instance_id != expected {
        return Err(
          crate::diagnostics::Error::new(
            if health.role != "control" {
              "connection.wrong_role"
            } else {
              "connection.wrong_instance"
            },
            "The responding service is not the discovered control daemon",
          )
          .fact("role", health.role)
          .fact("instance", health.instance_id)
          .check("Inspect the selected local service and discovery ownership; do not delete a live socket.")
          .into(),
        );
      }
    }
    Ok(match &self.credential {
      Some(token) => client.with_token(token)?,
      None => client,
    })
  }

  /// Non-secret argv that keeps a suggested operation on this selection.
  pub fn action_selection(&self, context: &Context, remote: bool) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(file) = &self.config_file {
      args.extend(["--config".into(), file.display().to_string()]);
    }
    if remote {
      if let Some(name) = &self.context {
        args.extend(["--context".into(), name.clone()]);
      } else if let Some(address) = &self.address {
        args.extend(["--address".into(), address.clone()]);
        if let Some(file) = &context.credential_file {
          args.extend(["--credential-file".into(), file.display().to_string()]);
        }
      }
    }
    args
  }

  /// Stable references for a same-account child that has access to this config.
  pub fn execution(&self, run: Option<String>) -> Result<Option<String>> {
    let Some(config_file) = &self.config_file else {
      return Ok(None);
    };
    // A direct bearer file has no saved identity reference. Do not inherit its
    // secret or claim a later command will be authenticated automatically.
    if self.context.is_none() && self.credential.is_some() {
      return Ok(None);
    }
    let config_file = if config_file.is_absolute() {
      config_file.clone()
    } else {
      std::env::current_dir()?.join(config_file)
    };
    Ok(Some(serde_json::to_string(&Execution {
      version: 1,
      config_file,
      context: self.context.clone(),
      address: self.address.clone(),
      command: self.command.clone(),
      run,
    })?))
  }
}

/// Explicit input wins over inherited selection, then the saved default.
pub fn resolve(ctx: &Context) -> Result<Resolved> {
  if let Some(address) = &ctx.address {
    needy_config::global::validate_address(address, false)?;
    let credential = match &ctx.credential_file {
      Some(path) => Some(crate::configuration::read_secret(path)?),
      None => None,
    };
    return Ok(Resolved {
      context: None,
      source: "--address",
      address: Some(address.clone()),
      command: None,
      credential,
      expected_instance: None,
      config_file: ctx.configuration_file().ok(),
    });
  }
  let inherited = if ctx.selected_context.is_none() {
    inherited()?
  } else {
    None
  };
  if ctx.selected_context.is_none() && inherited.is_none() {
    let legacy: Vec<_> =
      ["NEEDY_ENDPOINT", "NEEDY_TOKEN"].into_iter().filter(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty())).collect();
    if !legacy.is_empty() {
      return Err(crate::diagnostics::Error::new("config.selection_conflict", "Legacy endpoint/token selectors are no longer supported")
        .fact("variables", legacy.join(", "))
        .check("Select --context NAME or --address URI explicitly; save connections with config context and pass direct credentials through --credential-file. Unset the legacy variables.").into());
    }
  }
  let config_file = if ctx.config.is_none() {
    inherited.as_ref().map(|execution| execution.config_file.clone()).unwrap_or(ctx.configuration_file()?)
  } else {
    ctx.configuration_file()?
  };
  let config = crate::configuration::load(&config_file)?;
  let (name, source) = match (&ctx.selected_context, &inherited) {
    (Some(name), _) => (name.clone(), "--context"),
    (None, Some(execution)) => match &execution.context {
      Some(name) => (name.clone(), "inherited execution context"),
      None => {
        let address = execution.address.clone();
        let command = execution.command.clone();
        match (&address, &command) {
          (Some(a), None) => needy_config::global::validate_address(a, false)?,
          (None, Some(c)) if !c.is_empty() && !c[0].is_empty() => {}
          _ => bail!("invalid inherited transport; select --context or --address explicitly"),
        }
        return Ok(Resolved {
          context: None,
          source: "inherited execution context",
          address,
          command,
          credential: None,
          expected_instance: None,
          config_file: Some(config_file),
        });
      }
    },
    (None, None) => (config.current_context.clone(), "saved configuration"),
  };
  let (address, command, credential, expected_instance) = if name == "local" {
    #[cfg(unix)]
    {
      let mut paths = ctx.base_paths()?.with_config_file(config_file.clone());
      paths.apply(&config.paths);
      let record = needy_daemon::discovery::read(&paths.runtime)?;
      (Some(record.address), None, None, Some(record.instance_id))
    }
    #[cfg(not(unix))]
    {
      return Err(
        crate::diagnostics::Error::new("platform.unsupported", "Local control hosting is not supported on this platform yet")
          .check("Select a remote Linux or macOS control plane with --context or --address.")
          .into(),
      );
    }
  } else {
    let record = config.contexts.get(&name).ok_or_else(|| {
      crate::diagnostics::Error::new("config.context_not_found", "The saved connection does not exist")
        .fact("context", &name)
        .check("List connections with needy config context ls and select an existing name.")
    })?;
    (record.address.clone(), record.command.clone(), Some(record.credential.clone()), None)
  };
  if let Some(execution) = &inherited
    && (execution.address != address || execution.command != command)
  {
    bail!("the inherited connection transport changed; select --context or --address explicitly before continuing");
  }
  Ok(Resolved {
    context: Some(name),
    source,
    address,
    command,
    credential,
    expected_instance,
    config_file: Some(config_file),
  })
}

/// Check the built-in local management entry without borrowing a remote token.
pub async fn local(ctx: &Context) -> Result<Client> {
  let local = Context {
    config: ctx.config.clone(),
    selected_context: Some("local".into()),
    address: None,
    credential_file: None,
    diagnostic_format: ctx.diagnostic_format,
  };
  resolve(&local)?.connect().await
}
