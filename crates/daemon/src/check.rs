//! `profile check` through the control plane: where a profile's actor and
//! executors would run, whether its selectors resolve, and its recipe
//! checked against the CPU of the host its actor would run on. It creates
//! nothing.

use std::path::{Path, PathBuf};

use needy_api_server::control::{ControlError, Placement, ProfileCheck, ProfileFinding, ProfileToCheck};
use needy_config::Source;
use needy_config::validate::{self, Severity};
use needy_core::recipe::HostInfo;
use needy_core::types::RunnerRole;

use crate::{Daemon, failed, schedule};

fn error(message: String) -> ProfileFinding {
  ProfileFinding {
    error: true,
    message,
  }
}

fn warning(message: String) -> ProfileFinding {
  ProfileFinding {
    error: false,
    message,
  }
}

impl Daemon {
  pub(crate) async fn check_profile(&self, request: ProfileToCheck) -> Result<ProfileCheck, ControlError> {
    let source = Source::text(request.profile_yaml.clone(), PathBuf::new());
    let profile = needy_config::profile::load(&source).map_err(|e| ControlError::Invalid(format!("{:#}", anyhow::Error::from(e))))?;
    let mut check = ProfileCheck::default();
    check.findings.extend(validate::profile(&profile).into_iter().map(|f| ProfileFinding {
      error: f.severity == Severity::Error,
      message: f.message,
    }));
    let runners = self.candidates().await.map_err(failed)?;

    match schedule::placement(profile.actor.runner.as_ref(), RunnerRole::Actor, &runners) {
      Ok(placement) => {
        if let Some(refusal) = self.actor_refusal(&placement) {
          check.findings.push(error(format!("actor: {refusal}")));
        }
        check.actor = Some(placement);
      }
      Err(e) => check.findings.push(error(format!("actor: {e}"))),
    }
    if let Some(reference) = &profile.actor.recipe {
      let actor = check.actor.clone();
      self.check_recipe(reference, &request.recipe_yaml, actor.as_ref(), &mut check);
    }
    for (name, executor) in &profile.executors {
      match schedule::placement(executor.runner.as_ref(), RunnerRole::Executor, &runners) {
        Ok(placement) => {
          if let Err(refusal) = self.site(&placement, executor.provider) {
            check.findings.push(error(format!("executors.{name}: {refusal}")));
          }
          check.executors.insert(name.clone(), placement);
        }
        Err(e) => check.findings.push(error(format!("executors.{name}: {e}"))),
      }
    }
    Ok(check)
  }

  /// Compiles the recipe `reference` against the CPU of the host the actor
  /// would run on, as `prepare` does when the run is created, and adds its
  /// findings.
  fn check_recipe(&self, reference: &str, recipe_yaml: &str, actor: Option<&Placement>, check: &mut ProfileCheck) {
    // The client sends a recipe file's content; the daemon reads only its
    // built-in recipes.
    let source = match Source::reference(reference, Path::new("")) {
      builtin @ Source::Builtin(_) => builtin,
      _ if recipe_yaml.is_empty() => {
        check.findings.push(error(format!("recipe {reference} was not sent with the profile")));
        return;
      }
      _ => Source::text(recipe_yaml.to_string(), PathBuf::new()),
    };
    let recipe = match needy_config::recipe::load(&source) {
      Ok(recipe) => recipe,
      Err(e) => {
        check.findings.push(error(format!("recipe: {:#}", anyhow::Error::from(e))));
        return;
      }
    };
    let host = match actor {
      // An actor whose selector does not resolve has no host to check
      // against; the selector's finding says why.
      None => HostInfo::default(),
      Some(Placement::ControlHost) => needy_core_actor::host::detect::detect().unwrap_or_else(|e| {
        check.findings.push(warning(format!("cannot read the control host's CPU; the recipe's host checks are skipped: {e}")));
        HostInfo::default()
      }),
      Some(Placement::Runner(name)) => match self.runner_host(name) {
        Some(host) => host.cpu,
        None => {
          check.findings.push(warning(format!("runner {name} has not reported its host yet; the recipe's host checks are skipped")));
          HostInfo::default()
        }
      },
    };
    let compiled = needy_core::recipe::compile(&recipe, &host, 1);
    check.actor_cpu = host.model_name;
    check.findings.extend(compiled.findings.into_iter().map(|f| ProfileFinding {
      error: f.severity == Severity::Error,
      message: format!("recipe: {}", f.message),
    }));
  }
}

#[cfg(test)]
#[path = "check_test.rs"]
mod tests;
