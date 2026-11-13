//! Human and JSON infrastructure errors, kept separate from child output.
//! Producers provide stable reasons and facts; this boundary chooses advice.

use std::collections::BTreeMap;

use clap::ValueEnum;
use serde::Serialize;

#[derive(Clone, Copy, Default, ValueEnum)]
pub enum Format {
  #[default]
  Human,
  Json,
}

#[derive(Debug, thiserror::Error)]
#[error("{summary}")]
pub struct Error {
  reason: &'static str,
  summary: String,
  facts: BTreeMap<String, String>,
  checks: Vec<String>,
  completion: Option<&'static str>,
  #[source]
  cause: Option<anyhow::Error>,
}

impl Error {
  pub fn new(reason: &'static str, summary: impl Into<String>) -> Self {
    Self {
      reason,
      summary: summary.into(),
      facts: BTreeMap::new(),
      checks: Vec::new(),
      completion: None,
      cause: None,
    }
  }

  pub fn fact(mut self, key: &str, value: impl Into<String>) -> Self {
    self.facts.insert(key.into(), value.into());
    self
  }

  pub fn check(mut self, check: impl Into<String>) -> Self {
    self.checks.push(check.into());
    self
  }

  pub fn outcome_unknown(mut self) -> Self {
    self.completion = Some("unknown");
    self
  }

  pub fn cause(mut self, cause: impl Into<anyhow::Error>) -> Self {
    self.cause = Some(cause.into());
    self
  }
}

#[derive(Serialize)]
struct Check {
  summary: String,
  location: &'static str,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  argv: Vec<String>,
}

#[derive(Serialize)]
struct Diagnostic {
  version: u32,
  reason: String,
  summary: String,
  facts: BTreeMap<String, String>,
  completion: Option<&'static str>,
  checks: Vec<Check>,
}

/// Render only known safe structured facts. Unknown causes keep their family;
/// English error strings never decide authentication or recovery policy.
pub fn render_in(error: &anyhow::Error, format: Format, context: Option<&crate::cli::Context>) {
  let mut diagnostic = Diagnostic {
    version: 1,
    reason: "internal.unknown".into(),
    summary: format!("{error:#}"),
    facts: BTreeMap::new(),
    completion: None,
    checks: vec![],
  };
  for cause in error.chain() {
    if let Some(local) = cause.downcast_ref::<Error>() {
      diagnostic.reason = local.reason.into();
      diagnostic.summary = local.summary.clone();
      diagnostic.facts = local.facts.clone();
      diagnostic.completion = local.completion;
      diagnostic.checks = local.checks.iter().map(|text| check(text)).collect();
      break;
    }
    if let Some(config) = cause.downcast_ref::<needy_config::global::Error>() {
      diagnostic.reason = config.reason().into();
      diagnostic.facts.insert("field".into(), config.field().into());
      diagnostic
        .checks
        .push(check("Inspect this field in the selected global configuration; preserve saved credentials while correcting it."));
      break;
    }
    #[cfg(unix)]
    if let Some(shutdown) = cause.downcast_ref::<needy_daemon::IncompleteShutdown>() {
      diagnostic.reason = "daemon.cleanup_incomplete".into();
      diagnostic.facts.insert("runs".into(), shutdown.runs.join(","));
      diagnostic.checks.push(check(
        "Inspect the retained run state and daemon logs before restarting; keep incomplete mirror outputs and traces for reconciliation.",
      ));
      break;
    }
    #[cfg(unix)]
    if let Some(discovery) = cause.downcast_ref::<needy_daemon::discovery::Error>() {
      diagnostic.reason = discovery.reason().into();
      diagnostic
        .checks
        .push(check("Inspect `needy daemon service status`; start or install the local service, or use `needy daemon serve`."));
      break;
    }
    if let Some(api) = cause.downcast_ref::<needy_api_client::Error>() {
      use needy_api_client::Kind;
      let (reason, check) = match api.kind() {
        Kind::Unauthenticated => (
          "auth.credential_rejected",
          "Check the selected connection with `needy config context check`; ask its operator whether this identity was revoked.",
        ),
        Kind::Denied => (
          "auth.role_denied",
          "Inspect the selected caller and requested operation. Management requires local operator or an administrator invitation.",
        ),
        Kind::NotFound => {
          ("resource.not_found", "List the relevant runs, runners or processes on the selected control plane and verify the identifier.")
        }
        Kind::Invalid => ("request.invalid", "Check the command's --help and the named configuration field."),
        Kind::Failed => {
          ("operation.failed_precondition", "Inspect the actual execution target, working directory and required runtime before retrying.")
        }
        Kind::Exhausted => {
          ("capacity.exhausted", "Inspect the selected executor's capacity and running processes; do not repeat the request blindly.")
        }
        Kind::Unavailable => (
          "connection.unavailable",
          "Check the selected connection and its tunnel. Local services can be inspected with `needy daemon service status`.",
        ),
        Kind::Ended => ("process.outcome_unknown", "Inspect the process or run before starting it again; the command may have completed."),
        Kind::Other => {
          ("protocol.invalid_response", "Inspect the actual cause and client/server versions; do not reset state as a first step.")
        }
      };
      diagnostic.reason = reason.into();
      diagnostic.checks.push(self::check(check));
      // Metadata decoding is bounded in the API client. Dedicated reason/facts
      // may refine this family without replacing raw refusal details.
      if let Some(detail) = api.diagnostic() {
        diagnostic.reason = detail.reason;
        diagnostic.facts = detail.facts.into_iter().collect();
      }
      break;
    }
    if let Some(io) = cause.downcast_ref::<std::io::Error>() {
      diagnostic.reason = match io.kind() {
        std::io::ErrorKind::PermissionDenied => "filesystem.permission_denied",
        std::io::ErrorKind::NotFound => "filesystem.not_found",
        std::io::ErrorKind::AlreadyExists => "filesystem.conflict",
        _ => "filesystem.error",
      }
      .into();
      diagnostic.checks.push(check("Inspect the named path and its owner/permissions. Preserve existing state while checking the cause."));
    }
  }
  if diagnostic.reason == "process.outcome_unknown" {
    diagnostic.completion = Some("unknown");
  }
  if let Some(summary) = advice(&diagnostic.reason) {
    diagnostic.checks = vec![check(summary)];
  }
  let argv: &[&str] = match diagnostic.reason.as_str() {
    "process.outcome_unknown" | "process.not_found" => &["needy", "proc", "ls"],
    "service.not_installed" => &["needy", "daemon", "service", "install"],
    "discovery.not_found" | "discovery.invalid_record" | "discovery.stale_record" => &["needy", "daemon", "service", "status"],
    "auth.admin_required" | "auth.credential_rejected" => &["needy", "config", "context", "ls"],
    "run.not_found" | "run.ambiguous" | "run.ended" | "policy.exec_denied" | "fault.exec_injected" => &["needy", "run", "ls"],
    "runner.not_found" | "runner.offline" | "runner.not_connected" => &["needy", "runner", "ls"],
    "config.read_failed" | "config.write_failed" => &["needy", "config", "paths"],
    _ => &[],
  };
  if !argv.is_empty() {
    if diagnostic.checks.is_empty() {
      diagnostic.checks.push(check("Inspect the selected object before retrying."));
    }
    diagnostic.checks[0].argv = argv.iter().map(|part| (*part).into()).collect();
  }
  let mut secrets = Vec::new();
  if let Some(context) = context {
    if let Ok(config) = context.configuration() {
      secrets.extend(config.contexts.values().map(|entry| entry.credential.clone()));
    }
    if let Some(path) = &context.credential_file
      && let Ok(secret) = crate::configuration::read_secret(path)
    {
      secrets.push(secret);
    }
    if [
      "auth.",
      "connection.",
      "run.",
      "runner.",
      "process.",
      "executor.",
      "policy.",
      "fault.",
    ]
    .iter()
    .any(|prefix| diagnostic.reason.starts_with(prefix))
      && let Ok(selected) = crate::connection::resolve(context)
    {
      for check in &mut diagnostic.checks {
        if !check.argv.is_empty() {
          let remote = !matches!(check.argv.get(1).map(String::as_str), Some("config" | "daemon" | "image"));
          check.argv.splice(1..1, selected.action_selection(context, remote));
        }
      }
      if let Some(name) = selected.context {
        diagnostic.facts.entry("context".into()).or_insert(name);
      }
      diagnostic.facts.entry("selection_source".into()).or_insert(selected.source.into());
      diagnostic.facts.entry("transport".into()).or_insert_with(|| selected.address.unwrap_or_else(|| "connector".into()));
    }
  }
  let secrets: Vec<_> = secrets.iter().map(String::as_str).collect();
  diagnostic.summary = safe(&crate::configuration::safe_text(&diagnostic.summary, &secrets));
  for value in diagnostic.facts.values_mut() {
    *value = safe(&crate::configuration::safe_text(value, &secrets));
  }
  for check in &mut diagnostic.checks {
    check.summary = safe(&check.summary);
  }
  match format {
    Format::Json => match serde_json::to_string(&diagnostic) {
      Ok(text) => eprintln!("{text}"),
      Err(_) => eprintln!("{{\"version\":1,\"reason\":\"internal.render_failed\",\"summary\":\"Cannot render diagnostic\"}}"),
    },
    Format::Human => {
      eprintln!("needy: {} [{}]", diagnostic.summary, diagnostic.reason);
      for (key, value) in &diagnostic.facts {
        eprintln!("  {key}: {value}");
      }
      for check in &diagnostic.checks {
        eprintln!("  Check: {}", check.summary);
        if !check.argv.is_empty() {
          eprintln!("    {}: {}", check.location, shell_words::join(&check.argv));
        }
      }
    }
  }
}

fn check(summary: &str) -> Check {
  Check {
    summary: summary.into(),
    location: "client",
    argv: Vec::new(),
  }
}

/// Keep terminal control bytes inert and redact recognized URL userinfo even
/// when a peer embeds an address inside a longer error description.
fn safe(text: &str) -> String {
  let mut text = text.to_owned();
  for word in text.split_whitespace().map(str::to_owned).collect::<Vec<_>>() {
    let address = word.trim_matches(['`', '\'', '"', '(', ')', ',', ';']);
    let masked = crate::configuration::safe_text(address, &[]);
    if masked != address {
      text = text.replace(address, &masked);
    }
  }
  text
    .chars()
    .flat_map(|character| {
      if character.is_control() {
        character.escape_default().collect::<Vec<_>>()
      } else {
        vec![character]
      }
    })
    .collect()
}

/// Advice follows producer reasons, never substrings of a vendor message.
fn advice(reason: &str) -> Option<&'static str> {
  Some(match reason {
    "policy.exec_denied" => "Inspect the matching routing rule and run report; authorization does not override an execution policy.",
    "fault.exec_injected" => "Inspect the matching chaos fault and run report before retrying.",
    "auth.backend_unavailable" => {
      "Ask the control-plane operator to inspect credential-store availability and daemon logs; keep the current credential."
    }
    "auth.admin_required" => "Select a connection with management access or ask the control-plane operator for an administrator invitation.",
    "auth.role_denied" => {
      "Inspect the caller's client, actor or runner role and the requested API; a context alias does not grant authority."
    }
    "executor.authentication_failed" => {
      "Ask the operator to inspect the executor credential and control-plane configuration; client pairing does not repair executor access."
    }
    "process.cwd_invalid" => "Inspect the working directory, workspace and path map on the execution machine.",
    "process.start_failed" => "Inspect the executable and permissions on the execution machine.",
    "process.terminal_failed" => "Inspect terminal support on the execution machine, or explicitly select pipes with -T.",
    "process.internal_attach_denied" => "List visible processes; internal mounts and transfers cannot be attached.",
    "run.ended" => "Inspect this ended run's report or create a new run before executing.",
    "connection.unavailable" => {
      "Check the selected address, route or connector with needy config context check; preserve the selected target."
    }
    "bootstrap.invalid_code" | "bootstrap.expired" | "bootstrap.used" | "bootstrap.revoked" | "bootstrap.scope_denied" => {
      "Request a fresh invitation for the matching role and control plane."
    }
    "state.read_failed" | "state.write_failed" => {
      "Ask the operator to inspect the named store and daemon logs; preserve state and credentials."
    }
    _ => return None,
  })
}
