//! Temporary changes to the locally resolved profile submitted for one creation.

use anyhow::{Context as _, Result, bail};
use needy_config::{Source, profile};
use serde_yaml::Value;
use std::path::Path;

#[derive(clap::Args, Default)]
pub(super) struct Overrides {
  /// Replace the actor image for this creation.
  #[arg(long)]
  image: Option<String>,
  /// Replace the actor workspace path (inside the actor).
  #[arg(long)]
  workspace: Option<String>,
  /// Replace the workspace source: executor:NAME:PATH or host:PATH.
  #[arg(long)]
  workspace_from: Option<String>,
  /// Allow the actor to write the workspace source.
  #[arg(long)]
  workspace_writable: bool,
  /// Set one actor environment entry; repeat for multiple entries.
  #[arg(long = "env", value_name = "KEY=VALUE", value_parser = environment)]
  env: Vec<(String, String)>,
  /// Replace the driving program, using shell quoting to split its arguments.
  #[arg(long)]
  entry_command: Option<String>,
}

fn environment(text: &str) -> Result<(String, String), String> {
  let (key, value) = text.split_once('=').ok_or("an environment entry must be KEY=VALUE")?;
  if key.is_empty() || key.contains('\0') || value.contains('\0') {
    return Err("an environment entry needs a nonempty key and no NUL bytes".into());
  }
  Ok((key.into(), value.into()))
}

pub(super) fn resolve(path: &Path, overrides: Overrides) -> Result<profile::Resolved> {
  let resolved = profile::resolve(&Source::file(path))?;
  let mut document: Value = serde_yaml::from_str(&resolved.yaml)?;
  let actor = &mut document["actor"];
  let image_overridden = overrides.image.is_some();
  if let Some(image) = overrides.image {
    actor["image"] = image.into();
  }
  if let Some(workspace) = overrides.workspace {
    actor["workspace"]["path"] = workspace.into();
  }
  if let Some(from) = overrides.workspace_from {
    actor["workspace"]["from"] = from.into();
  }
  if overrides.workspace_writable {
    actor["workspace"]["writable"] = true.into();
  }
  for (key, value) in overrides.env {
    actor["env"][key] = value.into();
  }
  if let Some(command) = overrides.entry_command {
    let words = shell_words::split(&command).context("invalid --entry-command quoting")?;
    if words.is_empty() {
      bail!("--entry-command must name a program");
    }
    actor["command"] = serde_yaml::to_value(words)?;
  }
  // Resolve again so newly supplied relative host paths use the profile's
  // directory, exactly as paths already written in the profile do.
  let source = Source::text(serde_yaml::to_string(&document)?, path.parent().unwrap_or(Path::new("")).to_path_buf());
  let mut snapshot = profile::resolve(&source)?;
  snapshot.recipe_yaml = resolved.recipe_yaml;
  if !image_overridden {
    snapshot.image_file = resolved.image_file;
  }
  let loaded = profile::load(&Source::text(snapshot.yaml.clone(), path.parent().unwrap_or(Path::new("")).to_path_buf()))?;
  let errors: Vec<_> = needy_config::validate::profile(&loaded)
    .into_iter()
    .filter(|f| f.severity == needy_config::validate::Severity::Error)
    .map(|f| f.message)
    .collect();
  if !errors.is_empty() {
    bail!("invalid profile: {}", errors.join("; "));
  }
  Ok(snapshot)
}
