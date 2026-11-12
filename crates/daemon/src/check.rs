//! `profile check` through the control plane: where a profile's actor and
//! executors would run, whether its selectors resolve, whether the actor's
//! host has a usable KVM, and its recipe checked against the CPU of the host
//! its actor would run on. It creates nothing.

use std::path::{Path, PathBuf};

use crate::schedule::Need;
use needy_api_server::control::{ControlError, Placement, ProfileCheck, ProfileFinding, ProfileToCheck};
use needy_config::Source;
use needy_config::profile::{Executor, Profile, service_host};
use needy_config::validate::{self, Severity};
use needy_core::recipe::{CompiledRecipe, HostInfo};
use needy_core::types::Capture;

use crate::pool::Site;
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

/// The actor's working directory when no entry of `executor`'s path map
/// covers it, so a command routed there keeps a path of the actor's.
// NOTICE: the actor starts in `actor.workspace.path`, else in `~`; a
// workspace from this executor maps its path already (`profile::load`
// derives the entry).
fn unmapped_actor_cwd(profile: &Profile, executor: &Executor) -> Option<String> {
  let cwd = profile.actor.workspace.as_ref().map_or_else(|| profile.user.home(), |w| w.path.clone());
  let covers = |from: &str| {
    let from = from.trim_end_matches('/');
    cwd == from || cwd.starts_with(&format!("{from}/"))
  };
  (!executor.path_map.keys().any(|from| covers(from))).then_some(cwd)
}

/// Why a service's `url` cannot be reached from this machine, where the
/// gateway that forwards to it runs, or `None` when a TCP connection to it
/// opens. A URL `validate` refused already has its own finding.
async fn unreachable(url: &str) -> Option<String> {
  let uri: http::Uri = url.parse().ok()?;
  let host = uri.host()?.trim_start_matches('[').trim_end_matches(']').to_string();
  let port = uri.port_u16().unwrap_or(if uri.scheme_str() == Some("https") {
    443
  } else {
    80
  });
  // NOTICE: 2 s is a check's patience for a service on this machine or its
  // network, not a bound on how slowly a service may answer requests.
  match tokio::time::timeout(std::time::Duration::from_secs(2), tokio::net::TcpStream::connect((host.as_str(), port))).await {
    Ok(Ok(_)) => None,
    Ok(Err(e)) => Some(format!("does not accept a connection ({e})")),
    Err(_) => Some("does not answer within 2 s".into()),
  }
}

/// What an actor whose driver catches execs with shims does not route.
const CONTAINER_ROUTING_GAP: &str = "actor: a container actor routes by shims made when its image was built: a program \
  created at run time (installed with pip or npm, or compiled in the workspace) and run directly stays in the actor \
  whatever the rules say; programs on the image's PATH, and scripts through their interpreters, are routed";

/// Whether any of `profile`'s rules or exec faults applies to a command in
/// the actor.
fn routes_anything(profile: &Profile) -> bool {
  needy_core::rules::route_names(profile).is_none_or(|names| !names.is_empty())
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

    match schedule::placement(profile.actor.runner.as_ref(), Need::Actor(profile.actor.driver), &runners) {
      Ok(placement) => {
        if let Some(refusal) = self.actor_refusal(&placement) {
          check.findings.push(error(format!("actor: {refusal}")));
        } else if let Some(missing) = self.unusable_host(&placement, &profile).await {
          check.findings.push(error(format!("actor: {missing}")));
        }
        check.actor = Some(placement);
      }
      Err(e) => check.findings.push(error(format!("actor: {e}"))),
    }
    for (name, service) in &profile.actor.network.services {
      if let Some(problem) = unreachable(&service.url).await {
        check.findings.push(warning(format!(
          "actor.network.services.{name}: http://{} goes to {}, which {problem} from this control host, where the gateway runs",
          service_host(name),
          service.url
        )));
      }
    }
    if needy_core_actor::driver::facets(profile.actor.driver).capture == Capture::Shims && routes_anything(&profile) {
      check.findings.push(warning(CONTAINER_ROUTING_GAP.into()));
    }
    let recipe = match &profile.actor.recipe {
      Some(reference) => {
        let actor = check.actor.clone();
        self.check_recipe(reference, &request.recipe_yaml, actor.as_ref(), &mut check)
      }
      None => None,
    };
    // What of the identity the driver does not present, where the actor
    // would run: this host (see `unusable_host`).
    if check.actor.is_some()
      && let Ok(driver) = crate::actor::driver_for(self, &profile.actor)
    {
      for (is_error, message) in crate::actor::identity_findings(driver.as_ref(), recipe.as_ref()) {
        check.findings.push(ProfileFinding {
          error: is_error,
          message: format!("actor: {message}"),
        });
      }
    }
    for (name, executor) in &profile.executors {
      match schedule::placement(executor.runner.as_ref(), Need::Executor(executor.driver), &runners) {
        Ok(placement) => {
          match self.site(&placement, executor.driver) {
            Err(refusal) => check.findings.push(error(format!("executors.{name}: {refusal}"))),
            // The control host's own runner is the actor's machine.
            Ok(Site::Runner { name: runner, .. }) if !self.is_local(&runner) => {
              if let Some(cwd) = unmapped_actor_cwd(&profile, executor) {
                check.findings.push(warning(format!(
                  "executors.{name}: commands routed to runner {runner} keep the actor's working directory {cwd}, which \
                   no path_map entry maps to a directory on that machine; add one, for example \
                   `path_map: {{ {cwd}: /path/on/{runner} }}`"
                )));
              }
            }
            Ok(_) => {}
          }
          check.executors.insert(name.clone(), placement);
        }
        Err(e) => check.findings.push(error(format!("executors.{name}: {e}"))),
      }
    }
    Ok(check)
  }

  /// Why the host of `profile`'s actor placed at `placement` cannot
  /// run it (no usable KVM, for Firecracker), or `None` when it can. The
  /// actor runs on the control host (directly or as its own runner), which
  /// is probed as `run create` probes it.
  // TODO(runner-actor): an actor on another runner is refused before this
  // check (`actor_refusal`); once one can run there, check the KVM its
  // heartbeat reports (`RunnerHost::cpu.kvm_cpuid`, unset without one).
  async fn unusable_host(&self, placement: &Placement, profile: &Profile) -> Option<String> {
    let on = match placement {
      Placement::ControlHost => "the control host".to_string(),
      Placement::Runner(name) => format!("runner {name} (the control host)"),
    };
    // A driver this daemon lacks is already a finding of `validate::profile`.
    let driver = crate::actor::driver_for(self, &profile.actor).ok()?;
    driver.check_host().await.err().map(|e| format!("on {on}: {e}"))
  }

  /// Compiles the recipe `reference` against the CPU of the host the actor
  /// would run on, as `prepare` does when the run is created, and adds its
  /// findings.
  fn check_recipe(&self, reference: &str, recipe_yaml: &str, actor: Option<&Placement>, check: &mut ProfileCheck) -> Option<CompiledRecipe> {
    // The client sends a recipe file's content; the daemon reads only its
    // built-in recipes.
    let source = match Source::reference(reference, Path::new("")) {
      builtin @ Source::Builtin(_) => builtin,
      _ if recipe_yaml.is_empty() => {
        check.findings.push(error(format!("recipe {reference} was not sent with the profile")));
        return None;
      }
      _ => Source::text(recipe_yaml.to_string(), PathBuf::new()),
    };
    let recipe = match needy_config::recipe::load(&source) {
      Ok(recipe) => recipe,
      Err(e) => {
        check.findings.push(error(format!("recipe: {:#}", anyhow::Error::from(e))));
        return None;
      }
    };
    let host = match actor {
      // An actor whose selector does not resolve has no host to check
      // against; the selector's finding says why.
      None => HostInfo::default(),
      Some(Placement::ControlHost) => needy_core_actor::driver::linux_firecracker::kvm::detect().unwrap_or_else(|e| {
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
    let compiled = needy_core::recipe::compile(&recipe, &host, &needy_core::recipe::PerRun::at(1, needy_common::time::now_ms()));
    check.actor_cpu = host.model_name;
    check.findings.extend(compiled.findings.iter().map(|f| ProfileFinding {
      error: f.severity == Severity::Error,
      message: format!("recipe: {}", f.message),
    }));
    Some(compiled)
  }
}

#[cfg(test)]
#[path = "check_test.rs"]
mod tests;
