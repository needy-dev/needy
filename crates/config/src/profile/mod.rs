//! Profiles: one session's user, actor, executors, routing rules, networks,
//! file mappings, chaos and the trace.

mod actor;
mod chaos;
mod executor;
mod files;
mod network;
mod rules;
mod trace;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

pub use actor::{Actor, Limits};
pub use chaos::{Chaos, Fault, FaultKind, Readings};
pub use executor::{Docker, DockerBuild, Executor, OnFull, Remote, Runtime};
pub use files::{FileMapping, FileMode, Files, MirrorWhen, Side};
pub use network::{Injection, NetworkPolicy};
pub use rules::{Actions, ArgOps, EnvOps, Resolver, Rule, UnknownPolicy, When};
pub use trace::{Trace, TraceSink};

use crate::{Error, Kind, Source, merge, source};

/// One session: machines, routing rules, networks, files and chaos.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Profile {
  pub user: User,
  pub actor: Actor,
  pub executors: BTreeMap<String, Executor>,
  pub executor_network: NetworkPolicy,
  /// Named blocks of actions that rules reuse with `use:`.
  pub presets: BTreeMap<String, Actions>,
  /// Every rule that matches applies, in order.
  pub commands: Vec<Rule>,
  pub files: Files,
  /// The mappings this session uses; empty means all of them.
  pub session_files: Vec<String>,
  pub chaos: Chaos,
  pub resolvers: Vec<Resolver>,
  pub unknown: UnknownPolicy,
  /// Where commands that no rule places run; the actor when absent.
  pub default_executor: Option<String>,
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
  let mut profile: Profile = source::deserialize(document, Kind::Profile, source)?;
  profile.base_dir = source.base_dir();
  // `~` means the profile user's home, known only after the whole chain
  // merged.
  let user = profile.user.clone();
  for mapping in profile.files.mappings.values_mut() {
    mapping.from = mapping.from.clone().expand_home(&user);
    mapping.to = mapping.to.clone().expand_home(&user);
  }
  Ok(profile)
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
