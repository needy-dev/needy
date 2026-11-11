//! Profiles: one run's user, actor, executors, routing rules, networks,
//! file mappings, chaos and the trace.

mod actor;
mod chaos;
mod executor;
mod files;
mod network;
mod rules;
mod runner;
mod trace;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

pub use actor::{Actor, Limits, Runtime};
pub use chaos::{Chaos, Fault, FaultKind, Readings};
pub use executor::{Cpus, Device, Docker, DockerBuild, Executor, Gpus, MemorySwap, Mount, OnFull, Provider, Ulimit};
pub use files::{FileMapping, FileMode, Files, MirrorWhen, Side};
pub use network::{Injection, NetworkPolicy};
pub use rules::{Actions, ArgOps, EnvOps, Resolver, Rule, UnknownPolicy, When};
pub use runner::{LabelMatch, RunnerSelector};
pub use trace::{Trace, TraceSink};

use crate::{Error, Kind, Source, merge, source};

/// One run: machines, routing rules, networks, files and chaos.
// NOTICE: no container-level `#[serde(default)]`: `actor` is required, since
// its `runtime` is (see `Actor`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
  #[serde(default)]
  pub user: User,
  pub actor: Actor,
  #[serde(default)]
  pub executors: BTreeMap<String, Executor>,
  #[serde(default)]
  pub executor_network: NetworkPolicy,
  /// Named blocks of actions that rules reuse with `use:`.
  #[serde(default)]
  pub presets: BTreeMap<String, Actions>,
  /// Every rule that matches applies, in order.
  #[serde(default)]
  pub commands: Vec<Rule>,
  #[serde(default)]
  pub files: Files,
  /// The mappings this run uses; empty means all of them.
  #[serde(default)]
  pub run_files: Vec<String>,
  #[serde(default)]
  pub chaos: Chaos,
  #[serde(default)]
  pub resolvers: Vec<Resolver>,
  #[serde(default)]
  pub unknown: UnknownPolicy,
  /// Where commands that no rule places run; the actor when absent.
  #[serde(default)]
  pub default_executor: Option<String>,
  #[serde(default)]
  pub trace: Trace,
  /// The directory that relative paths in the profile resolve against.
  #[serde(skip)]
  pub base_dir: PathBuf,
}

/// The single user that the actor and every executor share.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct User {
  pub name: String,
  pub uid: u32,
  pub gid: u32,
}

impl Default for User {
  fn default() -> Self {
    Self {
      name: "agent".into(),
      uid: 1000,
      gid: 1000,
    }
  }
}

impl User {
  /// `/home/<name>`, the same on the actor and every executor.
  pub fn home(&self) -> String {
    format!("/home/{}", self.name)
  }

  /// Expands a leading `~` to [`User::home`].
  pub fn expand_home(&self, path: &str) -> String {
    let home = self.home();
    shellexpand::tilde_with_context(path, || Some(home.as_str())).into_owned()
  }
}

/// A profile's rules are evaluated child first.
const MERGE: merge::Rules = merge::Rules {
  prepend: &["commands", "resolvers"],
};

/// Reads a profile and its `extends` chain.
pub fn load(source: &Source) -> Result<Profile, Error> {
  let document = source::read_chain(source, Kind::Profile, &MERGE)?;
  refuse_removed(&document, source)?;
  let mut profile: Profile = source::deserialize(document, Kind::Profile, source)?;
  profile.base_dir = source.base_dir();
  // `~` means the profile user's home, known only after the whole chain
  // merged.
  let user = profile.user.clone();
  for mapping in profile.files.mappings.values_mut() {
    mapping.from = mapping.from.clone().expand_home(&user);
    mapping.to = mapping.to.clone().expand_home(&user);
  }
  // A `path_map` key is an actor path, so its `~` is the user's home there,
  // as on a file mapping's actor side; routing and `profile check` both read
  // the expanded key.
  // NOTICE: a value is a path on the executor's machine, whose home this
  // profile does not know, so it keeps a `~` as written.
  for executor in profile.executors.values_mut() {
    executor.path_map = std::mem::take(&mut executor.path_map).into_iter().map(|(from, to)| (user.expand_home(&from), to)).collect();
  }
  Ok(profile)
}

/// A profile as a client sends it to a control daemon: self-contained, so
/// the daemon reads no path on the client's machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
  /// The profile with its `extends` chain merged and its relative host
  /// paths made absolute.
  pub yaml: String,
  /// The actor's recipe with its `extends` chain merged, when `actor.recipe`
  /// names a file; a built-in recipe is the daemon's own.
  pub recipe_yaml: Option<String>,
}

/// Reads a profile, its `extends` chain and the recipe file it names into a
/// [`Resolved`] profile.
///
/// A host path (`host:` in a file mapping, an executor's `workspace`) is a
/// path on the control daemon's machine; a relative one is made absolute
/// against the profile's directory here.
// TODO(runner-files): relative host paths still resolve against the
// client's directory, which names the right files only when the client
// runs on the control daemon's machine. Moving files between a client and
// the control plane belongs to the runners and runs design
// (`docs/ai/references/architecture/2026-11-10-runner-and-run-design.md`).
pub fn resolve(source: &Source) -> Result<Resolved, Error> {
  let mut document = source::read_chain(source, Kind::Profile, &MERGE)?;
  refuse_removed(&document, source)?;
  let base = source.base_dir();
  absolute_host_paths(&mut document, &base);
  let profile: Profile = source::deserialize(document.clone(), Kind::Profile, source)?;
  let recipe_yaml = match &profile.actor.recipe {
    Some(reference) => match Source::reference(reference, &base) {
      Source::Builtin(_) => None,
      file => Some(crate::recipe::resolve(&file)?),
    },
    None => None,
  };
  Ok(Resolved {
    yaml: source::serialize(&document, Kind::Profile, source)?,
    recipe_yaml,
  })
}

/// Refuses the keys that runners and typed Docker fields replaced, naming
/// what replaces them, instead of the bare "unknown field" a removed key
/// would get.
// NOTICE: no compatibility (principle 7 of the module layout redesign):
// these keys are refused, not translated.
fn refuse_removed(document: &serde_yaml::Value, source: &Source) -> Result<(), Error> {
  use serde_yaml::Value;
  let Some(Value::Mapping(executors)) = document.get("executors") else {
    return Ok(());
  };
  for (name, executor) in executors {
    let name = name.as_str().unwrap_or_default();
    let message = if executor.get("remote").is_some() || executor.get("runtime").and_then(Value::as_str) == Some("remote") {
      let key = if executor.get("remote").is_some() {
        "remote"
      } else {
        "runtime"
      };
      Some((
        key,
        "remote executors were removed: join the machine as a runner (`needy runner token create`, then `needy daemon --mode runner`) \
         and select it with `runner: NAME`"
          .to_string(),
      ))
    } else if let Some(runtime) = executor.get("runtime") {
      let runtime = runtime.as_str().unwrap_or("…");
      Some(("runtime", format!("`runtime` is now `provider` (`provider: {runtime}`); select the machine with `runner:`")))
    } else if executor.get("docker").and_then(|docker| docker.get("args")).is_some() {
      Some((
        "docker.args",
        "`docker.args` was replaced by typed fields under `docker:`: cpus, cpu_shares, cpuset_cpus, memory, memory_swap, \
         shm_size, pids_limit, ulimits, env, workdir, read_only, init, mounts, add_host, dns, sysctls, gpus, cap_drop, and, \
         where the operator allows them, privileged, cap_add, devices, pid, ipc, userns, uts and security_opt"
          .to_string(),
      ))
    } else {
      None
    };
    if let Some((key, message)) = message {
      return Err(Error::Invalid {
        kind: Kind::Profile,
        origin: source.to_string(),
        path: format!("executors.{name}.{key}"),
        message,
      });
    }
  }
  Ok(())
}

/// Makes the relative host paths of a merged profile absolute against
/// `base`: `host:` sides of file mappings, executors' workspaces and the
/// sources of their Docker bind mounts.
// NOTICE: `executors.*.docker.build.context` is a host path too, but no
// provider builds images yet; it is left as written.
fn absolute_host_paths(document: &mut serde_yaml::Value, base: &Path) {
  use serde_yaml::Value;
  if let Some(Value::Mapping(executors)) = document.get_mut("executors") {
    for executor in executors.values_mut() {
      if let Some(Value::String(workspace)) = executor.get_mut("workspace") {
        *workspace = source::absolute(workspace, base);
      }
      if let Some(Value::Sequence(mounts)) = executor.get_mut("docker").and_then(|docker| docker.get_mut("mounts")) {
        for mount in mounts.iter_mut().filter(|mount| mount.get("type").and_then(Value::as_str) == Some("bind")) {
          if let Some(Value::String(source)) = mount.get_mut("source") {
            *source = source::absolute(source, base);
          }
        }
      }
    }
  }
  if let Some(Value::Mapping(files)) = document.get_mut("files") {
    for mapping in files.values_mut() {
      for side in ["from", "to"] {
        if let Some(Value::String(text)) = mapping.get_mut(side)
          && let Some(path) = text.strip_prefix("host:")
        {
          *text = format!("host:{}", source::absolute(path, base));
        }
      }
    }
  }
}

/// A profile's name from its file: the file name without `.yaml` or `.yml`
/// and then without `.profile`. A file named only `profile.yaml` takes its
/// directory's name, so `examples/smoke/profile.yaml` is `smoke` and
/// `examples/bench/mac.profile.yaml` is `mac`.
pub fn name(path: &Path) -> String {
  let file = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
  let stem = file.strip_suffix(".yaml").or_else(|| file.strip_suffix(".yml")).unwrap_or(&file);
  let stem = stem.strip_suffix(".profile").unwrap_or(stem);
  if (stem == "profile" || stem.is_empty())
    && let Some(dir) = path.parent().and_then(Path::file_name)
  {
    return dir.to_string_lossy().into_owned();
  }
  stem.to_string()
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
